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
// The pairing half of citation trust: a real label cited next to a value that
// lives in a DIFFERENT chunk (gen75 NARA misattribution). Consumed in
// `gate_held_answer` after the label-fidelity pass.
pub(crate) use citation_attribution::align_citation_values;

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

use judge::{
    claim_violation_joint, extract_claim_list, scan_unsupported_specifics,
    unwrap_unverified_excerpts,
};

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
    /// Per-chunk labels PARALLEL to `chunks` (`gate_evidence_chunk_labels`) —
    /// the mapping the citation-value ALIGNMENT check needs (WHICH chunk does a
    /// cited label name). Empty when unavailable — alignment is then skipped.
    pub chunk_labels: Vec<Vec<String>>,
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

/// Per-chunk citation labels, PARALLEL to `gate_evidence_chunks` (same raptor
/// exclusion, same order): `chunk_labels[i]` = the labels (title first, then
/// corpus id) a `[Source: …]` naming chunk `i` would use. The flat
/// `source_labels` cannot reconstruct this mapping, and the citation-value
/// ALIGNMENT check (`align_citation_values`) needs it: WHICH chunk does the
/// cited label name, so the citing segment's values can be verified against
/// that chunk specifically.
pub(crate) fn gate_evidence_chunk_labels(
    chunks: &[corpus_engine::ScoredChunk],
) -> Vec<Vec<String>> {
    let exclude = std::env::var("SOVEREIGN_GATE_EXCLUDE_RAPTOR")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    chunks
        .iter()
        .filter(|c| !(exclude && c.metadata.get("source").map(String::as_str) == Some("raptor")))
        .map(|c| {
            let mut labels = Vec::with_capacity(2);
            if let Some(t) = c.title.as_deref() {
                let t = t.trim();
                if !t.is_empty() {
                    labels.push(t.to_string());
                }
            }
            let cid = c.corpus_id.trim();
            if !cid.is_empty() {
                labels.push(cid.to_string());
            }
            labels
        })
        .collect()
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
///
/// Wording is a SELF-SCOPED epistemic hedge ("I couldn't confirm …"), NOT a
/// universal claim about the sources ("none of them cover it"). Measured
/// 2026-07-08 (8h chaos run, class-A "evidence-denial"): the gate's short
/// citation path abstains far more often than the evidence warrants (single-digit
/// answers filtered, verbatim quote-match misses), so the abstention frequently
/// fires when the answer IS in the passages. A universal negative is then a FALSE
/// statement about the sources — the trust rubric scores it as confabulation, and
/// it reads to the user as the app denying its own evidence. An assistant-scoped
/// "I couldn't verify this against them" is honest in BOTH the true-miss and the
/// mis-abstain case (it claims only the assistant's confidence, never the
/// sources' content), and the calibrated judge's decline-shape override already
/// treats it as an honest limitation rather than a fabrication.
pub(crate) fn grounded_abstention(_claim: &str, chunks_checked: usize) -> String {
    format!(
        "I couldn't confirm an answer to this against the {chunks_checked} passages \
         your sources turned up — so rather than guess at something I can't verify \
         from them, I'd flag that instead. If you think it's there, try rephrasing \
         with the specific names or terms involved and I'll take another look."
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
        if let Some(after) = text[p..].split_once(':').map(|x| x.1) {
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

/// Live claim-check progress out of the gate ladder — the frames the
/// desktop's verification panel renders (claims stamped one by one).
/// The receiver (streaming's `gate_held_answer`) forwards each frame
/// as a `turn-narration` event. Emission is `try_send` throughout:
/// perception, never backpressure — a full channel drops the frame and
/// the judge calls proceed untouched. `None` everywhere except the
/// streaming spawns keeps every other gated surface byte-identical.
pub(crate) type GateProgressSender = tokio::sync::mpsc::Sender<crate::types::NarrationPhase>;

/// Fire-and-forget progress emit (see `GateProgressSender`).
fn emit_gate_progress(progress: Option<&GateProgressSender>, frame: crate::types::NarrationPhase) {
    if let Some(tx) = progress {
        let _ = tx.try_send(frame);
    }
}

/// Wire-safe claim text for progress frames: the UI stamps one row per
/// claim, so a bounded prefix is enough (full texts stay in gate meta).
fn wire_claim(claim: &str) -> String {
    const CAP: usize = 160;
    if claim.chars().count() <= CAP {
        claim.to_string()
    } else {
        let mut s: String = claim.chars().take(CAP).collect();
        s.push('…');
        s
    }
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
    gate_answer_with_progress(inference, question, draft, evidence, base_request, profile, None)
        .await
}

/// `gate_answer` plus a live claim-check progress channel (see
/// `GateProgressSender`). The streaming spawns call this form; all
/// other surfaces keep the plain `gate_answer` signature.
pub(crate) async fn gate_answer_with_progress(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    use crate::types::NarrationPhase;
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
        return gate_longform(
            inference,
            question,
            draft,
            evidence,
            base_request,
            profile,
            progress,
        )
        .await;
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
    if config::citation_grounding_enabled() && (entity_anchored || config::citation_broad_enabled())
    {
        if let citation::CitationOutcome::Grounded { answer, quote } =
            citation::citation_grounded_answer(
                &**inference,
                question,
                chunks,
                crate::slot_policy::posture_of(base_request),
            )
            .await
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
            // Second-opinion fabrication guard: the citation path grounds the
            // asserted VALUE against a quote, but a confabulated quote wearing a
            // real-passage shape can still slip a fabricated named entity
            // through (measured: "David Hart, COO of Knowledge Process Software"
            // over Enron evidence). Scan the asserted answer holistically; on a
            // flag, correct-or-abstain instead of releasing the fabrication.
            if let Some(guarded) = short_specifics_guard(
                inference,
                question,
                &answer,
                chunks,
                evidence.searcher.as_ref(),
                base_request,
                profile,
            )
            .await
            {
                return guarded;
            }
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
    // Whether the short path actually extracted and judged a claim —
    // gates the ClaimCheckComplete frame (a NO_CLAIM release audited
    // nothing, so reporting "1 claim confirmed" would be a lie).
    let mut claim_audited = false;
    dbg(&format!(
        "gate_answer entity_anchored={entity_anchored} chunks={} draft={:?}",
        chunks.len(),
        text.chars().take(240).collect::<String>()
    ));
    // Structural exemption-closing: strip any GK caveat before extraction so the
    // asserted claim is actually verified rather than exempted as NO_CLAIM. This
    // runs UNCONDITIONALLY now (not just entity_anchored): gate_answer only fires
    // on the gated path, where documents WERE retrieved (gate_on requires
    // documents_found > 0), so the grounding contract applies — a "from general
    // knowledge" escape hatch must not ship confident specifics the retrieved
    // evidence can't support (observed 2026-07-01: "Winnie's former lover was
    // Eddie Henderson", a name absent from the Secret-Agent corpus, shipped under
    // a GK caveat on a non-entity-anchored question). If the GK content is
    // genuinely unsupported the gate now abstains — an honest "the sources don't
    // cover it" beats a labelled-but-confident fabrication. strip_gk_caveat is a
    // no-op when there is no caveat, so grounded answers are unaffected. The
    // released `text` is unchanged; only what the verifier reads is de-caveated.
    // Env-gated (SOVEREIGN_EXACTVAL_FIX=0 restores the prior entity_anchored-only
    // strip) for a clean replay A/B.
    let verify_text = if entity_anchored || config::exactval_fix_enabled() {
        strip_gk_caveat(&text)
    } else {
        text.clone()
    };
    match verify_grounding(
        inference,
        question,
        &verify_text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            dbg(&format!(
                "  verify: vp={:.3} tau={tau} claim={:?}",
                v.violation_prob,
                v.claim
                    .as_deref()
                    .map(|c| c.chars().take(70).collect::<String>())
            ));
            // Short path audits one central claim — surface it and its
            // verdict on the progress channel (extraction + judging is
            // one verify call here, so the frames land together).
            if let Some(c) = v.claim.as_deref() {
                claim_audited = true;
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckStart {
                        claims: vec![wire_claim(c)],
                        recheck: false,
                    },
                );
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimVerdict {
                        index: 0,
                        supported: v.violation_prob < tau,
                    },
                );
            }
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    if !profile.retry {
                        // Verify-only surfaces (Refinement): no second
                        // synthesis — the caller decides what replaces
                        // the failed text (typically: keep the prior
                        // verified answer).
                        text = grounded_abstention(&claim, chunks.len().min(12));
                        action = "abstained_no_retry";
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimCheckComplete {
                                confirmed: 0,
                                flagged: 1,
                            },
                        );
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
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckComplete {
                                    confirmed: 0,
                                    flagged: 1,
                                },
                            );
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
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimRevisionStart { failed: 1 },
                    );
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
                            // Same structural strip on the retry, matching the
                            // first-pass strip above (env-gated): the documented
                            // leak is a retry that re-asserts the fabrication
                            // wearing the GK caveat and slips the exemption.
                            let verify_second = if entity_anchored || config::exactval_fix_enabled()
                            {
                                strip_gk_caveat(&second)
                            } else {
                                second.clone()
                            };
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckStart {
                                    claims: vec![wire_claim(&claim)],
                                    recheck: true,
                                },
                            );
                            match verify_grounding(
                                inference,
                                question,
                                &verify_second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
                                crate::slot_policy::posture_of(base_request),
                            )
                            .await
                            {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
                                    text = second;
                                    action = "retry_released";
                                    emit_gate_progress(
                                        progress,
                                        NarrationPhase::ClaimVerdict {
                                            index: 0,
                                            supported: true,
                                        },
                                    );
                                }
                                Some(v2) => {
                                    final_vp = Some(v2.violation_prob);
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = "abstained";
                                    emit_gate_progress(
                                        progress,
                                        NarrationPhase::ClaimVerdict {
                                            index: 0,
                                            supported: false,
                                        },
                                    );
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
    // Terminal progress frame for the short path. Only when a claim
    // was actually audited (NO_CLAIM releases verified nothing) and
    // only on the verdicts this fall-through exit owns — the abstain
    // early-returns above emit their own completion frames.
    if claim_audited {
        let (confirmed, flagged) = match action {
            "released" => (1, 0),
            "retry_released" | "retry_released_unverified" => (1, 1),
            a if a.starts_with("abstained") => (0, 1),
            _ => (0, 0),
        };
        if confirmed + flagged > 0 {
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete { confirmed, flagged },
            );
        }
    }
    dbg(&format!(
        "verdict action={action} retried={retried} vp={final_vp:?} tau={tau}"
    ));
    tracing::info!(
        target: "grounding_gate",
        action,
        retried,
        vp = ?final_vp,
        tau,
        top_similarity = ?evidence.top_similarity,
        "grounding gate verdict"
    );
    // Fragment guard (gen75c: the answer to a three-variable code question was
    // the single word "Start", released via NO_CLAIM — a fragment extracts no
    // claim, so the verify ladder waves it through). A released answer this
    // short, with no grounding suffix and no decline shape, answers nothing:
    // convert it to the honest abstention instead of shipping noise. Terse
    // GROUNDED answers are unaffected — the citation path formats them with
    // their supporting quote, well past this floor.
    if action == "released"
        && text.trim().chars().count() < 15
        && !text.contains("Grounded in the source")
        && question.trim().chars().count() > 40
    {
        dbg(&format!(
            "fragment guard: released text {:?} answers nothing — abstaining",
            text.trim()
        ));
        return GateOutcome {
            text: grounded_abstention(question, chunks.len().min(12)),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "abstained_fragment",
                "retried": retried,
                "violation_prob": final_vp,
                "threshold": tau,
                "mode": "single_claim",
                "draft": draft_for_meta,
            }),
        };
    }
    // Second-opinion fabrication guard on a RELEASED single-claim answer — the
    // per-claim verify grounds the load-bearing value but is blind to fabricated
    // SUPPORTING specifics (a cited flag/number/entity absent from the
    // evidence). Skip when the path already abstained (nothing asserted). On a
    // flag: correct-or-abstain via one grounded rewrite.
    if !action.starts_with("abstained") && !action.starts_with("judge_failed") {
        if let Some(guarded) = short_specifics_guard(
            inference,
            question,
            &text,
            chunks,
            evidence.searcher.as_ref(),
            base_request,
            profile,
        )
        .await
        {
            return guarded;
        }
    }
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
/// User-facing wording (grace audit 2026-07-11): the previous prefix
/// ("From the retrieved sources, here is what can be established:")
/// injected auditor-speak as the OPENING of every rewritten answer — a
/// structural jargon hit on the grace gate's `clean` component. The
/// prefix's decode-commit job (force continuation into the supported
/// account) needs no machinery reference.
pub const LONGFORM_REWRITE_PREFIX: &str = "Here's what I can say with confidence:\n\n";

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
         passages actually state, citing them — do not merely delete it. Never add \
         a NEW statement about what the sources say, cite, name, or omit unless a \
         passage above shows it. Structure \
         the rewrite as an ANSWER, not a disclaimer: open directly with the \
         supported account, organized to address the question. Do not open with \
         what the sources lack, and do not enumerate the removed assertions in the \
         body. If material gaps remain, note them briefly in a single short \
         paragraph at the end."
    )
}

/// The user-visible verification note. Items are answer spans / short claims
/// (`normalize_scan_item` reduces scan output toward answer wording); render
/// each one deduped and length-capped, in plain language — judge vocabulary
/// must never reach the user (observed 2026-07-01: raw scan chatter footnoted
/// a released answer with "… is a fabricated specific").
///
/// Items are deliberately UNQUOTED: the post-synthesis quote guardrail
/// (`quote_verification::verify_answer_against_evidence`, streaming.rs) treats
/// any curly-quoted span as a quotation claim and demotes what it can't
/// verbatim-confirm — a quoted note item (a paraphrased claim, by nature not
/// verbatim) was rewritten to "[unverified excerpt: …]", turning the app's own
/// footer into a self-contradiction (probed 2026-07-01: the note trace showed
/// clean items; the released text showed them wrapped).
/// EXPERIMENT (`SOVEREIGN_NOTE_AS_METADATA=1`): keep the verification note
/// OUT of the answer text — the failed claims already ride
/// `GateOutcome.meta.failed_claims` → `metadata.grounding_gate`, and the
/// desktop renders them as a collapsible disclosure instead. Persona-QA
/// receipts (2026-07-11): the appended note owns the answer's final words
/// ("— The evidence states…", "[unverified excerpt:…]"), which zeroes the
/// grace gate's `agency`/`clean` components and buries the model's own
/// closing line — the honest audit trail read as auditor-speak in user
/// space. Default OFF: non-desktop surfaces (API/CLI) keep the in-text
/// note so a known-failed claim is never silently released without its
/// caveat (the never-silent invariant).
fn append_note(text: String, note: &str) -> String {
    let as_metadata = std::env::var("SOVEREIGN_NOTE_AS_METADATA")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if as_metadata {
        text
    } else {
        format!("{text}{note}")
    }
}

fn verification_note(failed_claims: &[String]) -> String {
    const NOTE_ITEM_CHARS: usize = 160;
    let mut seen = std::collections::HashSet::new();
    let items: Vec<String> = failed_claims
        .iter()
        .map(|c| {
            let c = unwrap_unverified_excerpts(c);
            let c = c.trim().trim_matches(['"', '“', '”']).trim();
            let mut item: String = c.chars().take(NOTE_ITEM_CHARS).collect();
            if c.chars().count() > NOTE_ITEM_CHARS {
                item.push('…');
            }
            item
        })
        .filter(|c| !c.is_empty() && seen.insert(c.to_lowercase()))
        .map(|c| format!("- {c}"))
        .collect();
    tracing::info!(
        target: "grounding_gate",
        n_claims = failed_claims.len(),
        n_items = items.len(),
        first_claim_head = %failed_claims.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        first_item_head = %items.first().map(|c| c.chars().take(80).collect::<String>()).unwrap_or_default(),
        "verification note rendered"
    );
    format!(
        "\n\n---\n*Verification note: these statements could not be confirmed \
         against your sources — treat them as unverified:*\n{}",
        items.join("\n")
    )
}

/// Cross-passage support check for ONE long-form claim: the top
/// passages are presented TOGETHER and the judge answers whether they
/// jointly state or imply the claim. Long-form synthesis legitimately
/// assembles claims across passages — per-chunk max-support is
/// structurally biased against exactly that (the bench critic's
/// documented blind spot; measured v13: a correct maximal essay was
/// rewritten into hedging because its synthesis claims had no single

/// Claim-audit budget for an answer of `chars` characters. Scales with
/// length so a long "exhaustive" answer — which buries fabricated specifics
/// in its later sections, past the first few load-bearing claims — gets
/// proportionate checking, instead of the fixed 4-claim audit that was
/// structurally blind to body fabrication (observed 2026-06-30: 3/5 shipped
/// fabrications were direct releases whose fabricated specifics were never
/// extracted). Floored at the surface's `min_claims` and capped so per-claim
/// judge latency stays bounded on very long answers.
pub(super) fn claim_budget(chars: usize, min_claims: usize) -> usize {
    // 600 chars/claim (not 900) so the empirical fabrication distribution
    // actually scales: the fixed-1h shipped fabrications sat at 3630-8571
    // chars, which at 900/claim only reached budget 4-9 (the 3630 case got
    // NO lift) — measured under-powered on the fab-fix run-1 trace. At 600 the
    // same cases get 6-10 claims audited. Cap 10 bounds the per-claim judge
    // latency on the longest answers (each audited claim is a 35B judge call).
    const MAX_AUDITED_CLAIMS: usize = 10;
    const CHARS_PER_CLAIM: usize = 600;
    (chars / CHARS_PER_CLAIM).clamp(min_claims, MAX_AUDITED_CLAIMS)
}

/// Whether the holistic supporting-specifics scan runs alongside the per-claim
/// audit in `gate_longform`. ON by default; `SOVEREIGN_SPECIFICS_SCAN=0`
/// disables it (the clean A/B lever — the per-claim audit alone is the prior
/// behaviour). The scan is one extra judge call per audited text; it catches
/// the fabricated specifics / misattributions the load-bearing claim extraction
/// structurally misses.
fn specifics_scan_enabled() -> bool {
    !matches!(
        std::env::var("SOVEREIGN_SPECIFICS_SCAN").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// Whether the SHORT-path second-opinion specifics scan runs. SHELVED — OFF by
/// default; opt in with `SOVEREIGN_SHORT_SPECIFICS_SCAN=1`. The short-path
/// "fabrication" category it targets proved to be ~90% measurement artifact
/// (correctly-grounded answers mis-scored because the offline evidence was
/// truncated); once that capture bug was fixed the guard's live A/B was no
/// longer a meaningful composite lever, so it ships dormant as defense-in-depth
/// pending a fresh clean-evidence validation. Kept separate from
/// `SOVEREIGN_SPECIFICS_SCAN` (the long-form scan, ON) so each band is
/// independently switchable.
fn short_specifics_scan_enabled() -> bool {
    matches!(
        std::env::var("SOVEREIGN_SHORT_SPECIFICS_SCAN")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// True when a released short answer is itself an honest abstention / decline
/// ("the sources don't cover it", "I'm not certain", the `grounded_abstention`
/// prose). Such an answer asserts no verifiable value, so the specifics scan has
/// nothing to fabricate-check — running it only surfaces kind-(3) noise (the
/// scan second-guessing a correct "not in sources" as a false claim ABOUT the
/// evidence). Skipping is a latency optimisation and errs fail-open: a false
/// skip just preserves prior behaviour. Measured 2026-07-01: 6/7 short-band scan
/// flags on GOOD answers were exactly these honest abstentions.
fn answer_declines(text: &str) -> bool {
    let h = text.trim_start().to_lowercase();
    const DECLINES: &[&str] = &[
        "i don't have reliable information",
        "i do not have reliable information",
        "i am not certain",
        "i'm not certain",
        "i do not have information",
        "i don't have information",
        "couldn't confirm an answer", // grounded_abstention prose (current)
        "could not confirm an answer", // grounded_abstention prose (current)
        "none of them actually cover it", // grounded_abstention prose (legacy, still in-the-wild)
        "i'd rather not guess",       // grounded_abstention prose (legacy)
        "do not contain",
        "does not contain",
        "not recorded there",
        "the sources do not",
        "the sources don't",
        "sources do not contain",
        "no passage",
        "not in your sources",
    ];
    DECLINES.iter().any(|d| h.contains(d))
}

/// Second-opinion fabrication guard for the SHORT gate path (single-claim +
/// citation). Those paths verify the LOAD-BEARING value but are structurally
/// blind to fabricated SUPPORTING specifics — a named person/flag/number/quote
/// the answer cites to `[Source: …]` that is absent from the evidence (observed
/// 2026-07-01 on thin evidence: "David Hart, COO of Knowledge Process Software"
/// shipped by the citation path, and tokei "--files"/"--sort"/".tokeignore"
/// specifics padded onto a grounded top-line). Runs the holistic specifics scan
/// on an already-RELEASED short answer; on a flag it routes into ONE corrective
/// retry (each flagged specific re-searched so the rewrite has the truth) and
/// re-scans the result, abstaining only if the rewrite still fabricates.
///
/// Never a blunt abstention: a truly-grounded specific gets its passage back and
/// the rewrite keeps it (self-correcting away a false positive), and a
/// mostly-grounded answer with one bad specific is rewritten, not discarded.
/// Returns `None` to leave the release unchanged — disabled, no-retry surface,
/// abstention-shaped answer, judge failure, or a clean scan.
#[allow(clippy::too_many_arguments)]
async fn short_specifics_guard(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    released: &str,
    chunks: &[String],
    searcher: Option<&Arc<dyn SealedEvidenceSearch>>,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> Option<GateOutcome> {
    // Only on retry-capable surfaces: the guard's whole remedy is a corrective
    // re-synthesis. Verify-only surfaces have no second synthesis to give.
    if !profile.retry {
        return None;
    }
    // Deterministic sentence sweep FIRST — receipt-grade hits (a code
    // identifier or in-world name attribution absent from the entire
    // evidence) trigger the corrective retry regardless of the LLM-scan flag.
    // gen75e s34: the `cmd_init`/`found.rs` ghosts shipped in a 1,504-char
    // answer — UNDER the 1,800 longform pivot, where none of the longform
    // vetoes run. The short path needs the same receipts-grade guard.
    let hay_lower = chunks.join(" ").to_lowercase();
    // Budget for the LLM scan paths (initial when the flag is on; the
    // post-retry re-scan always).
    let budget = claim_budget(released.chars().count(), 3);
    let mut swept: Vec<String> = Vec::new();
    for sentence in released.split(['.', '\n']) {
        let sentence = sentence.trim();
        if sentence.chars().count() < 20 {
            continue;
        }
        if let Some(x) = judge::absent_identifier_attribution(sentence, &hay_lower)
            .or_else(|| judge::absent_name_attribution(sentence, &hay_lower))
        {
            if !swept.contains(&x) {
                swept.push(x);
            }
        }
    }
    let specifics = if !swept.is_empty() {
        dbg(&format!(
            "short sweep VETOED {swept:?} (absent from evidence)"
        ));
        swept
            .iter()
            .map(|x| {
                format!("The answer references \"{x}\", which does not appear in the sources.")
            })
            .collect()
    } else {
        if !short_specifics_scan_enabled() {
            return None;
        }
        // Nothing asserted → nothing to fabricate-check (fail-open latency skip).
        if answer_declines(released) {
            return None;
        }
        // Small budget floored at 3 so even a terse citation answer ("David Hart")
        // gets a real check; scales modestly on longer short answers.
        let specifics = scan_unsupported_specifics(
            inference,
            question,
            released,
            chunks,
            budget,
            crate::slot_policy::posture_of(base_request),
        )
        .await?;
        if specifics.is_empty() {
            return None; // clean — release unchanged
        }
        specifics
    };
    // Corrective evidence per flagged specific — the same material the long-form
    // rewrite gets, and the self-correction for a false positive (a real
    // specific's grounding passage comes back, so the rewrite keeps it).
    let mut corrective: Vec<String> = Vec::new();
    if let Some(s) = searcher {
        for spec in specifics.iter().take(4) {
            if let Some(hit) = s.search(spec).await.into_iter().next() {
                corrective.push(hit);
            }
        }
    }
    let joined = specifics.join("\"; \"");
    dbg(&format!(
        "short_specifics_guard: {} flagged specific(s) [{:?}] → corrective retry",
        specifics.len(),
        joined.chars().take(90).collect::<String>()
    ));
    let mut retry_req = base_request.clone();
    let base_sys = retry_req.system_message.clone().unwrap_or_default();
    retry_req.system_message = Some(format!(
        "{base_sys}{}",
        retry_system_note(&joined, &corrective)
    ));
    retry_req.assistant_prefix = None;
    let second = match inference.complete(&retry_req).await {
        Ok(r) => r.text,
        Err(e) => {
            tracing::warn!(
                target: "grounding_gate",
                error = %e,
                "short specifics guard retry failed — keeping prior release"
            );
            return None; // fail-open: keep the original release
        }
    };
    // Re-scan the rewrite. Still fabricating → abstain; clean → release the
    // corrected answer. A re-scan judge failure falls open to keep the rewrite
    // (written under the corrective note, no worse than the flagged draft).
    match scan_unsupported_specifics(
        inference,
        question,
        &second,
        chunks,
        budget,
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) if !v.is_empty() => {
            tracing::info!(
                target: "grounding_gate",
                action = "abstained_specifics",
                flagged = specifics.len(),
                "short specifics guard: rewrite still fabricates — abstaining"
            );
            Some(GateOutcome {
                text: grounded_abstention("", chunks.len().min(12)),
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "abstained_specifics",
                    "retried": true,
                    "flagged_specifics": specifics,
                    "mode": "short_specifics",
                }),
            })
        }
        _ => {
            tracing::info!(
                target: "grounding_gate",
                action = "retry_released_specifics",
                flagged = specifics.len(),
                "short specifics guard: corrective rewrite released"
            );
            Some(GateOutcome {
                text: second,
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "retry_released_specifics",
                    "retried": true,
                    "flagged_specifics": specifics,
                    "mode": "short_specifics",
                }),
            })
        }
    }
}

/// Long-form ladder: per-claim audit → one rewrite → annotate.
/// An essay with one bad claim is REWRITTEN, not abstained; if the
/// rewrite still carries unsupported claims, they are listed in a
/// visible verification note appended to the answer — the reader sees
/// exactly which assertions didn't verify, instead of either losing
/// the whole essay or trusting it blind.
#[allow(clippy::too_many_arguments)]
async fn gate_longform(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    use crate::types::NarrationPhase;
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    let per_claim_chunks = profile.max_chunks;
    let min_claims = profile.max_claims;
    // Session posture for the judge envelopes, resolved once from the
    // synthesis turn's request; the audit closure captures it by copy.
    let posture = crate::slot_policy::posture_of(base_request);
    let audit = |text: String, recheck: bool| {
        let inference = inference.clone();
        let searcher = evidence.searcher.clone();
        let evidence_labels = evidence.source_labels.clone();
        async move {
            // Budget scales with THIS text's length — audited afresh for the
            // draft and again for the (possibly different-length) rewrite.
            let budget = claim_budget(text.chars().count(), min_claims);
            let claims = extract_claim_list(&inference, question, &text, budget, posture).await?;
            // Progress: the extracted claim list opens (or re-opens,
            // on the rewrite's re-audit) the desktop's check panel.
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckStart {
                    claims: claims.iter().take(budget).map(|c| wire_claim(c)).collect(),
                    recheck,
                },
            );
            let mut failed: Vec<FailedClaim> = Vec::new();
            // Evidence + labels, lowercased once, for the deterministic
            // in-world attribution veto below.
            let hay_lower = {
                let mut h = chunks.join(" ").to_lowercase();
                for l in &evidence_labels {
                    h.push(' ');
                    h.push_str(&l.to_lowercase());
                }
                h
            };
            for (claim_idx, claim) in claims.iter().take(budget).enumerate() {
                // Jurisdiction: honesty meta-language is not a world-claim —
                // "the system does not have access to X" can never be stated
                // by a passage, and auditing it prosecutes the answer's own
                // honesty (observed: refined honest declines rejected at vp
                // 0.85–0.98 on exactly these sentences). Deterministic shape
                // check; see is_self_referential_decline.
                if judge::is_self_referential_decline(claim) {
                    dbg(&format!(
                        "longform claim EXEMPT — self-referential decline: {claim:?}"
                    ));
                    // Exempt = ships unflagged; stamp the row so the
                    // panel never shows a permanently-pending claim.
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: true,
                        },
                    );
                    continue;
                }
                // Deterministic pre-check: an in-world attribution naming a
                // person absent from the ENTIRE evidence is fabricated — do
                // not ask the yes-biased joint judge (measured: "Betty
                // Alexander sent an email…" cleared at vp=0.010 despite the
                // name existing nowhere in the corpus; it shipped in 3 runs).
                let vetoed = judge::absent_name_attribution(claim, &hay_lower)
                    .map(|n| ("person", n))
                    .or_else(|| {
                        judge::absent_identifier_attribution(claim, &hay_lower)
                            .map(|i| ("identifier", i))
                    });
                if let Some((kind, name)) = vetoed {
                    dbg(&format!(
                        "longform claim VETOED — in-world attribution names {kind} {name:?}, absent from evidence: {claim:?}"
                    ));
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: false,
                        },
                    );
                    let extra = match &searcher {
                        Some(s) => s.search(claim).await,
                        None => Vec::new(),
                    };
                    failed.push(FailedClaim {
                        claim: claim.clone(),
                        evidence: extra,
                    });
                    continue;
                }
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
                match claim_violation_joint(&inference, claim, &judged, cap, posture).await {
                    Some(vp) => {
                        dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimVerdict {
                                index: claim_idx,
                                supported: vp < tau,
                            },
                        );
                        if vp >= tau {
                            failed.push(FailedClaim {
                                claim: claim.clone(),
                                evidence: extra,
                            });
                        }
                    }
                    None => {
                        // Unverifiable claim — fail open per claim; the
                        // row still resolves (it ships unflagged).
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimVerdict {
                                index: claim_idx,
                                supported: true,
                            },
                        );
                    }
                }
            }
            // Holistic supporting-specifics scan: catches the fabricated
            // details the load-bearing claim extraction misses (misattribution,
            // fake values, phantom section refs). One extra judge pass over the
            // WHOLE text vs the FULL evidence; its findings join `failed` and
            // ride the same rewrite/annotate path. Each flagged specific gets a
            // claim-conditioned search so the rewrite has corrective material —
            // which ALSO self-corrects a false positive: a truly-grounded
            // specific gets its grounding passage back, so the rewrite keeps it.
            if specifics_scan_enabled() {
                if let Some(specifics) =
                    scan_unsupported_specifics(&inference, question, &text, chunks, budget, posture)
                        .await
                {
                    for spec in specifics {
                        // Citations are validated by the deterministic snap pass
                        // BEFORE this audit — a scan finding about a `[Source:]`
                        // marker is out of its jurisdiction (observed 2026-07-01:
                        // the scan flagged REAL label citations, which then read
                        // as self-indictment in the verification note).
                        if spec.to_lowercase().contains("[source:") {
                            continue;
                        }
                        // Same jurisdiction rule as the claim loop: the
                        // answer's own honesty meta-language is exempt.
                        if judge::is_self_referential_decline(&spec) {
                            continue;
                        }
                        // Skip specifics already surfaced by the per-claim audit.
                        if failed
                            .iter()
                            .any(|f| f.claim.contains(&spec) || spec.contains(&f.claim))
                        {
                            continue;
                        }
                        let corrective = match &searcher {
                            Some(s) => s.search(&spec).await,
                            None => Vec::new(),
                        };
                        dbg(&format!(
                            "specifics_scan flagged {:?} (corrective_hits={})",
                            spec.chars().take(60).collect::<String>(),
                            corrective.len()
                        ));
                        failed.push(FailedClaim {
                            claim: spec,
                            evidence: corrective,
                        });
                    }
                }
            }
            // Sentence-level identifier sweep: the vetoes above only see
            // EXTRACTED claims, and ghost identifiers ride non-load-bearing
            // sentences the extractor never surfaces (gen75d s2: `cmd_init` /
            // `found.rs`, receipt-absent from the corpus, released inside a
            // rewrite despite the claim-level veto). Sweep every sentence of
            // the text with the same scoped checks; hits become synthetic
            // failed claims and ride the existing rewrite/annotate ladder.
            for sentence in text.split(['.', '\n']) {
                let sentence = sentence.trim();
                if sentence.chars().count() < 20 {
                    continue;
                }
                let hit = judge::absent_identifier_attribution(sentence, &hay_lower)
                    .or_else(|| judge::absent_name_attribution(sentence, &hay_lower));
                if let Some(ident) = hit {
                    if failed.iter().any(|f| f.claim.contains(&ident)) {
                        continue;
                    }
                    dbg(&format!(
                        "longform sentence sweep VETOED {ident:?} (absent from evidence)"
                    ));
                    let synthetic = format!(
                        "The answer references \"{ident}\", which does not appear in the sources."
                    );
                    let extra = match &searcher {
                        Some(s) => s.search(&synthetic).await,
                        None => Vec::new(),
                    };
                    failed.push(FailedClaim {
                        claim: synthetic,
                        evidence: extra,
                    });
                }
            }
            Some((text, claims.len(), failed))
        }
    };

    let draft_backup = draft.clone();
    let Some((text, n_claims, failed)) = audit(draft, false).await else {
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
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims,
                flagged: 0,
            },
        );
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
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(failed.len()),
                flagged: failed.len(),
            },
        );
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = verification_note(&failed_claims);
        return GateOutcome {
            text: append_note(text, &note),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "annotated_no_retry", "retried": false,
                "claims_checked": n_claims, "failed_claims": failed_claims,
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    dbg(&format!(
        "longform rewrite: {} failed of {n_claims}",
        failed.len()
    ));
    emit_gate_progress(
        progress,
        NarrationPhase::ClaimRevisionStart {
            failed: failed.len(),
        },
    );
    let mut rewrite_req = base_request.clone();
    let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
    rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
    rewrite_req.assistant_prefix = Some(LONGFORM_REWRITE_PREFIX.to_string());
    // A corrective rewrite prunes/replaces the failed claims — it must only be
    // able to TIGHTEN the draft, never regrow it into runaway fabrication
    // (observed 2026-06-30: the rewrite inherited the base "exhaustive/1500-word"
    // budget and inflated the answer x2-x7.5 — once to 23.8k chars of gibberish —
    // after which the re-audit released the enlarged fabrication). ~4 chars/token
    // is the usual English ratio. Budget 1.5x the draft's token estimate, not
    // 1.0x: a faithful rewrite REPLACES a short false claim with a LONGER cited
    // correction ("do not merely delete… cite them"), so a 1.0x cap starves it
    // and it ships truncated — the rewrite is non-streaming, so
    // continue_truncated_synthesis never repairs it (observed 2026-07-12: a
    // 2296-char draft's rewrite hit completion==max_tokens==574 and shipped cut
    // off mid-sentence; two other rewrites the same run finished at ~50% of their
    // caps, so 1.5x is a ceiling the rewrite won't pad to, not a target). 1.5x
    // stays well under the 2x floor of the runaway pathology, and the re-audit
    // still runs on the result — the extra headroom cannot smuggle a fabrication
    // past the gate. Floor keeps a short draft's rewrite from being starved.
    let draft_token_budget = (draft_backup.chars().count() * 3 / 8).max(256);
    rewrite_req.max_tokens = Some(
        rewrite_req
            .max_tokens
            .map_or(draft_token_budget, |m| m.min(draft_token_budget)),
    );
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
            match audit(second, true).await {
                Some((text2, n2, failed2)) if failed2.is_empty() => {
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimCheckComplete {
                            confirmed: n2,
                            flagged: 0,
                        },
                    );
                    GateOutcome {
                        text: text2,
                        meta: serde_json::json!({
                            "surface": profile.surface.id(),
                            "action": "rewrite_released", "retried": true,
                            "claims_checked": n2, "failed_claims": [],
                            "threshold": tau, "mode": "per_claim",
                        }),
                    }
                }
                Some((text2, n2, failed2)) => {
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimCheckComplete {
                            confirmed: n2.saturating_sub(failed2.len()),
                            flagged: failed2.len(),
                        },
                    );
                    let failed_claims: Vec<String> = failed2.into_iter().map(|f| f.claim).collect();
                    let note = verification_note(&failed_claims);
                    GateOutcome {
                        text: append_note(text2, &note),
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
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n_claims.saturating_sub(failed.len()),
                    flagged: failed.len(),
                },
            );
            let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
            let note = verification_note(&failed_claims);
            GateOutcome {
                text: append_note(text, &note),
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
    use crate::types::CompletionResponse;
    use crate::types::{Depth, ProviderCapabilities};
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
            // P4-D contract: every judge call routed through this mock must
            // carry the OICP Judge envelope and NOT the old
            // `model_id: "primary"` pin (a latent privacy hole). This is the
            // capture-stub assertion — it fires on the real gate paths the
            // tests below drive (claim extraction + forced-choice support).
            assert!(
                request.model_id.is_none(),
                "P4-D: judge request must not pin model_id; got {:?}",
                request.model_id
            );
            let judge_oicp = request
                .oicp
                .as_ref()
                .expect("P4-D: judge request must carry an OICP Judge envelope");
            assert_eq!(
                judge_oicp.effective_latency_class(),
                crate::oicp::LatencyClass::Normal,
                "P4-D: Judge envelope latency class"
            );
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
            } else if request.prompt.contains("List the SPECIFIC factual claims") {
                // Longform per-claim extractor (gate_longform's audit).
                "The shop is located on Crescent Lane.\nThe shop sells loose-leaf tea.".to_string()
            } else if request.prompt.contains("Compare the ANSWER against the EVIDENCE") {
                // Specifics scan: nothing unsupported — keeps the
                // longform progress tests pinned to the claim loop.
                "NONE".to_string()
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
            chunk_labels: Vec::new(),
            searcher: None,
            entity_anchored: false,
            top_similarity: None,
        }
    }

    #[test]
    fn claim_budget_scales_with_length_within_bounds() {
        // Short answers keep the floor (the surface's min_claims).
        assert_eq!(claim_budget(0, 4), 4);
        assert_eq!(claim_budget(500, 4), 4);
        assert_eq!(claim_budget(2_399, 4), 4, "under 4*600 stays at floor");
        // The empirical fabrication distribution now scales meaningfully — at
        // the old 900/claim these got budget 4-9 (3630 got NO lift).
        assert_eq!(
            claim_budget(3_630, 4),
            6,
            "3630-char fabrication -> 6, not 4"
        );
        assert_eq!(claim_budget(4_550, 4), 7, "4550-char fabrication -> 7");
        // Very long answers are capped so per-claim judge latency stays bounded.
        assert_eq!(
            claim_budget(8_571, 4),
            10,
            "8571-char essay -> capped at 10"
        );
        assert_eq!(claim_budget(usize::MAX, 4), 10);
        // The floor is the surface's min, not a hardcoded 4.
        assert_eq!(claim_budget(500, 1), 1);
    }

    #[test]
    fn answer_declines_skips_honest_abstentions_only() {
        // The exact short-band answers the specifics scan flagged as GOOD-but-
        // FLAGGED on 2026-07-01 — all honest abstentions the guard must SKIP so
        // it never wastes a corrective retry re-abstaining them.
        for decline in [
            "I don't have reliable information on the specific four authors listed for Chapter E.",
            "I am not certain of the value of `SWAP_THRESHOLD`. The provided sources do not contain this.",
            "The provided knowledge base sources do not contain this specific constant or file.",
            "I looked through the 12 passages your sources turned up for this, but none of them actually cover it — so I'd rather not guess.",
            "Based on the provided knowledge base, I do not have information regarding a character named \"Winnie\".",
            "The provided Rust snippets do not contain any assignment to a variable named `b`.",
            grounded_abstention("x", 12).as_str(),
        ] {
            assert!(answer_declines(decline), "should skip decline: {decline:?}");
        }
        // Real ASSERTING short answers the guard MUST scan — including the two
        // confirmed fabrications the guard exists to catch.
        for assert_ans in [
            "David Hart\n\nGrounded in the source: \"David Hart, Chief Operations Officer, Knowledge Process Software\"",
            "The most important thing is what Tokei does: it shows file-level stats (`--files`) and sorting (`--sort`).",
            "The three operations are index_stats, extract_shard, and merge_shards.",
        ] {
            assert!(!answer_declines(assert_ans), "should scan assertion: {assert_ans:?}");
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
        // "so I'm not going to state one" lecture for a warm, helpful refusal,
        // then re-scoped (2026-07-08) from a universal negative about the sources
        // ("none of them cover it") to a self-scoped hedge ("I couldn't confirm")
        // so a mis-abstain isn't a FALSE claim about the sources. The action is
        // the invariant; the wording is graceful and source-honest.
        assert!(outcome.text.starts_with("I couldn't confirm"));
        assert!(!outcome.text.contains("not going to state"));
        // Must NOT assert a universal negative about the sources' content.
        assert!(!outcome.text.contains("none of them"));
        assert!(!outcome.text.contains("not recorded there"));
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

    /// Drain every frame the gate pushed onto a progress channel.
    async fn drain_frames(
        mut rx: tokio::sync::mpsc::Receiver<crate::types::NarrationPhase>,
    ) -> Vec<crate::types::NarrationPhase> {
        let mut frames = Vec::new();
        while let Some(f) = rx.recv().await {
            frames.push(f);
        }
        frames
    }

    /// Short path, supported claim: the progress channel carries the
    /// desktop verification panel's contract — claim list opens, the
    /// verdict stamps, the completion frame closes the pass.
    #[tokio::test]
    async fn short_path_progress_frames_on_release() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let outcome = gate_answer_with_progress(
            &inference,
            "Where is the shop?",
            "The shop is on Harbour Row.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        let frames = drain_frames(rx).await;
        assert!(
            matches!(
                &frames[0],
                NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 1
            ),
            "first frame should open the one-claim check: {frames:?}"
        );
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: true
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 1,
                flagged: 0
            }
        ));
        assert_eq!(frames.len(), 3);
    }

    /// Short path, verify-only failure: verdict stamps unsupported and
    /// the completion frame reports the flagged claim.
    #[tokio::test]
    async fn short_path_progress_frames_on_abstention() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: false });
        let profile = GateSurface::Refinement.profile();
        assert!(!profile.retry);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let outcome = gate_answer_with_progress(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_no_retry")
        );
        let frames = drain_frames(rx).await;
        assert!(matches!(
            &frames[0],
            NarrationPhase::ClaimCheckStart { recheck: false, .. }
        ));
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: false
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 0,
                flagged: 1
            }
        ));
    }

    /// Longform path: the audit opens with the extracted claim LIST,
    /// stamps every claim in order, and closes with the totals — the
    /// full counter-card Check-station sequence.
    #[tokio::test]
    async fn longform_progress_frames_stamp_each_claim() {
        use crate::types::NarrationPhase;
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        // Force the per-claim ladder regardless of the profile's pivot
        // (and of any SOVEREIGN_LONGFORM_CHARS ambient override — the
        // draft is longer than both).
        let pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(profile.longform_chars)
            .max(profile.longform_chars);
        let draft = "The shop sits on Harbour Row, by the quay. ".repeat(pivot / 40 + 2);
        assert!(draft.chars().count() > pivot);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let outcome = gate_answer_with_progress(
            &inference,
            "Tell me about the shop.",
            draft,
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
            Some(&tx),
        )
        .await;
        drop(tx);
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        let frames = drain_frames(rx).await;
        // Claim list (two mock claims), then one verdict per claim in
        // index order, then the completion totals.
        assert!(
            matches!(
                &frames[0],
                NarrationPhase::ClaimCheckStart { claims, recheck: false } if claims.len() == 2
            ),
            "expected two-claim list first: {frames:?}"
        );
        assert!(matches!(
            frames[1],
            NarrationPhase::ClaimVerdict {
                index: 0,
                supported: true
            }
        ));
        assert!(matches!(
            frames[2],
            NarrationPhase::ClaimVerdict {
                index: 1,
                supported: true
            }
        ));
        assert!(matches!(
            frames[3],
            NarrationPhase::ClaimCheckComplete {
                confirmed: 2,
                flagged: 0
            }
        ));
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
        // The rewrite must not mint new claims about the sources (observed
        // 2026-07-01: a rewrite replaced one misattribution with "the text
        // cites Woolf's work" — a fresh unsupported claim ABOUT the text).
        assert!(note.contains("Never add a NEW statement about what the sources say"));
    }

    #[test]
    fn verification_note_dedupes_caps_and_stays_unquoted() {
        let long = "x".repeat(200);
        let claims = vec![
            "Paul Samuelson admitted defeat around 1963".to_string(),
            "\"Paul Samuelson admitted defeat around 1963\"".to_string(), // dup modulo quotes
            "[unverified excerpt: ships cannot pay tolls at sea]".to_string(),
            long.clone(),
            String::new(),
        ];
        let note = verification_note(&claims);
        // Deduped: the claim appears once, as a plain list item.
        assert_eq!(note.matches("Samuelson").count(), 1);
        assert!(note.contains("- Paul Samuelson admitted defeat around 1963"));
        // The app's own wrapper is unwrapped to its content.
        assert!(note.contains("- ships cannot pay tolls at sea"));
        assert!(!note.contains("unverified excerpt:"));
        // UNQUOTED by design: a curly-quoted item reads as a quotation claim to
        // the post-synthesis quote guardrail, which demotes non-verbatim spans
        // to "[unverified excerpt: …]" — mangling the note (probed 2026-07-01).
        assert!(!note.contains('“') && !note.contains('”'));
        // Long item capped with an ellipsis; empty item dropped.
        assert!(note.contains(&format!("{}…", "x".repeat(160))));
        // Plain language — never judge vocabulary.
        assert!(!note.to_lowercase().contains("fabricated"));
    }
}
