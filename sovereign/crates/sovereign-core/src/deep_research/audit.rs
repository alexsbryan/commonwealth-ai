// SPDX-License-Identifier: AGPL-3.0-or-later
//! R3 — the gap audit: the composed gate + claim splitting + gap
//! formation.
//!
//! The composed gate (gate-redesign.md §1) per claim:
//! 1. empty evidence window → **never-ran** (never a pass — §18.1);
//! 2. single-string judge (`claim_violation_joint`) — `None` (judge
//!    failed to run) → **could-not-judge**, recorded, never defaulted;
//! 3. `p >= tau` → **failed** (action `abstained_decline`);
//! 4. `p < tau` → judge-supported → **ref-required** (order
//!    deep-research-t4a): the draft must cite the chunks it asserts
//!    against — no citation handle → **could-not-judge**
//!    (`refused_no_citation_handle`); a handle naming no window chunk
//!    → **could-not-judge** (`refused_unresolvable_handle`);
//! 5. judge-supported + referenced → **containment witness** on the
//!    claim's extracted specifics against the REFERENCED chunk set;
//!    all witnessable specifics absent → downgrade to
//!    **could-not-judge** (the shared-bias residual);
//! 6. custody veto (R-3): a claim whose supporting chunks carry unknown
//!    provenance refuses (`refused_unknown_provenance`).
//! 7. corroboration floor (GAP-2/F22): a claim passes only if its
//!    support set spans ≥2 distinct provenance origins (distinct
//!    source_urls, C-class); a one-origin set caps at could-not-judge
//!    (`corroboration_floor`), the record verdict-visible.
//!
//! The witness only downgrades, and the floor only downgrades; the
//! ref-required stage adds refusal paths, never converts a verdict. The
//! same claim splitter feeds the R3 round audits and the R9 final
//! verdict set — one splitter, two consumers.

use super::containment::{citation_handles, containment_witness, ContainmentConfig};
use super::icd::{
    ClaimVerdict, CorroborationRecord, EmptyRoundReason, EmptyWindow, EvidenceWindow, FetchFailure,
    Gap, GapList, GateAction, Verdict, WitnessRecord,
};
use crate::oicp::ShardingPrivacy;
use crate::runtime::grounding::{claim_violation_joint, grounding_gate_threshold};
use crate::traits::InferenceProvider;
use std::sync::Arc;

/// One window chunk as the audit sees it (content + custody).
#[derive(Debug, Clone)]
pub struct AuditChunk {
    pub id: String,
    pub content: String,
    /// `None` = unknown provenance (refuses).
    pub custody_known: bool,
    pub source_url: String,
}

/// The audit result for one claim.
#[derive(Debug, Clone)]
pub struct ClaimAudit {
    pub claim: String,
    pub verdict: Verdict,
    pub action: GateAction,
    pub witness: WitnessRecord,
    /// The chunks whose content actually contains a supporting specific
    /// (the citations, C-class located).
    pub supporting_chunk_ids: Vec<String>,
    pub empty_evidence_window: bool,
    pub reason: Option<String>,
    /// GAP-2 — the corroboration floor's record (F22): present when the
    /// claim reached the floor, on both the cap and the pass.
    pub corroboration: Option<CorroborationRecord>,
}

impl ClaimAudit {
    pub fn is_gap(&self) -> bool {
        matches!(self.verdict, Verdict::CouldNotJudge | Verdict::NeverRan)
    }
}

/// Deterministic claim splitter: sentence boundaries, with trailing
/// `[Source: …]` spans attached to their sentence. Used by R3 (round
/// drafts) and R9 (final draft) — one splitter.
/// The span the model placed at the END of a sentence, before its final
/// period ("…strategies [Source: ev-1].") — the model's real shape. The
/// attach branch handles spans AFTER the punctuation; this captures the
/// before-the-period shape and names it the paragraph's span. Mid-sentence
/// spans (followed by more prose) are claim-local, never paragraph spans.
fn trailing_span(text: &str) -> Option<String> {
    let start = text.rfind("[Source:")?;
    let after = &text[start..];
    let close = after.find(']')?;
    let tail = after[close + 1..].trim();
    if tail.is_empty() || tail.chars().all(|c| matches!(c, '.' | '!' | '?')) {
        Some(after[..=close].to_string())
    } else {
        None
    }
}

/// t6b (red-first, pre-registered): flush a paragraph's buffered claims —
/// each untagged claim inherits the paragraph's span. The model writes ONE
/// span per paragraph (typically at its end); the witness still verifies
/// each claim's figures against the referenced chunk, so an inherited span
/// routes the claim INTO verification, never around it.
fn flush_paragraph(paragraph: &mut Vec<String>, span: &Option<String>, claims: &mut Vec<String>) {
    for mut c in paragraph.drain(..) {
        if !c.contains("[Source:") {
            if let Some(sp) = span {
                c.push_str(&format!(" {sp}"));
            }
        }
        claims.push(c);
    }
}

/// Markdown heading lines are STRUCTURE, not assertions — they are
/// replaced with a blank line (already a paragraph boundary here) before
/// the sentence splitter runs.
///
/// Two defects on the first live composed flight (2026-08-23, run
/// dr-1787534265) traced to this being absent:
///   1. The deliverable's own H1 is `# {question}`, and a research question
///      ends in `?` — sentence-final punctuation. The title was extracted as
///      a claim, judged, and the rendered report led with
///      `# <the user's question> **[refuted by the evidence]**`. A question
///      is not an assertion and cannot be refuted.
///   2. `compose_report` emits `### Heading\nFirst sentence.` with no blank
///      line between, so the heading was absorbed into the following
///      sentence and appeared inside claim text in the Verification list.
///      `synthesize.rs::count_header_swallows` exists to detect this shape
///      on the drafting side; nothing enforced it on the audit side.
///
/// Only ATX headings at line start count (`#` through `######` followed by
/// space or end-of-line). A `#` mid-line — "issue #42", a C preprocessor
/// line inside prose — is untouched.
fn blank_out_heading_lines(draft: &str) -> String {
    draft
        .split('\n')
        .map(|line| {
            let t = line.trim_start();
            let hashes = t.len() - t.trim_start_matches('#').len();
            let is_atx =
                (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ');
            if is_atx {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn split_claims(draft: &str) -> Vec<String> {
    let mut claims = Vec::new();
    let mut current = String::new();
    // The model's span sits at the paragraph's END — the paragraph's
    // claims are buffered and flushed at the paragraph boundary, so the
    // earlier sentences inherit it (a live last-seen span cannot reach
    // back to sentences already pushed).
    let mut paragraph: Vec<String> = Vec::new();
    let mut paragraph_span: Option<String> = None;
    // Iterate char-wise, splitting on sentence-final punctuation
    // (., !, ?) followed by whitespace or end.
    let draft = blank_out_heading_lines(draft);
    let chars: Vec<char> = draft.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        current.push(c);
        // Blank line = paragraph boundary: flush with the paragraph's
        // span, then reset it.
        if c == '\n' && chars.get(i + 1) == Some(&'\n') {
            flush_paragraph(&mut paragraph, &paragraph_span, &mut claims);
            paragraph_span = None;
        }
        let is_sentence_end = matches!(c, '.' | '!' | '?');
        if is_sentence_end {
            // Sentence-final punctuation is followed by whitespace or
            // EOF. Mid-token periods (URL dots inside a sentence or a
            // span) must not split.
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j == i + 1 && j < chars.len() {
                // No whitespace after the punctuation — a mid-token
                // period (e.g. "example.com/a"). Keep scanning.
                i += 1;
                continue;
            }
            // Peek: span-attached sentences — "[Source: x]" after the end.
            let k = j;
            let span_head: &[char] = &['[', 'S', 'o', 'u', 'r', 'c', 'e', ':'];
            let is_span = k < chars.len() && chars[k..].starts_with(span_head);
            let mut attached = false;
            if is_span {
                // Attach the WHOLE span — '[' through its closing ']'.
                // (Look the closing bracket up without moving `k` first:
                // a prior consume-to-']' pass would leave nothing for
                // the attach to copy.) Unterminated spans attach nothing
                // and the sentence ends at the punctuation.
                if let Some(close) = chars[k..].iter().position(|&c| c == ']') {
                    let end = k + close;
                    let span: String = chars[k..=end].iter().collect();
                    paragraph_span = Some(span.clone());
                    current.extend(chars[k..=end].iter());
                    i = end + 1;
                    attached = true;
                    // The span-closing period: "…1873. [Source: x]."
                    // — the '.' immediately after ']' completes the
                    // claim's sentence and must not become a stray
                    // claim of its own.
                    if i < chars.len() && matches!(chars[i], '.' | '!' | '?') {
                        current.push(chars[i]);
                        i += 1;
                    }
                }
            }
            if !attached {
                i = j.max(i + 1);
            }
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                if let Some(sp) = trailing_span(&trimmed) {
                    paragraph_span = Some(sp);
                }
                paragraph.push(trimmed);
            }
            current.clear();
        } else {
            i += 1;
        }
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        if let Some(sp) = trailing_span(&tail) {
            paragraph_span = Some(sp);
        }
        paragraph.push(tail);
    }
    flush_paragraph(&mut paragraph, &paragraph_span, &mut claims);
    claims
}

/// The composed gate over one claim. `tau` is read at run start from
/// `grounding_gate_threshold()` and frozen into the charter hash — the
/// loop re-reads nothing mid-run (FR-3).
#[allow(clippy::too_many_arguments)]
/// The ONE decider over the round window's own fields (order
/// deep-research-t7b, pre-registered — §10.6: one implementation per
/// threshold; §2: closed sets are enums). A window that ADDED evidence
/// is None; the five empty shapes are the closed enum. The round's own
/// window is per-round (NEW chunks only), so "refused everything"
/// reads as empty-chunks + empty-failures + non-empty dedup_refused —
/// the t6c pinned shape.
///
/// drb1-r1 Item 2: Distinguishes `RetriesExhausted` (fetch failures
/// after retries) from `Failed` (immediate failures).
pub fn empty_round_reason(window: &EvidenceWindow) -> Option<EmptyRoundReason> {
    if !window.chunks.is_empty() {
        return None;
    }
    let failed = !window.fetch_failures.is_empty();
    let refused = !window.dedup_refused.is_empty();
    // drb1-t2: pages WERE fetched but every one was content-refused —
    // its own named shape (budget spent, no evidence; the refusals
    // carry their reasons on the window).
    let content_refused = !window.content_refused.is_empty();

    // drb1-r1 Item 2: Check if any failure exhausted retries
    let retries_exhausted = window.fetch_failures.iter().any(|f| f.retries > 0);

    match (failed, refused, content_refused, retries_exhausted) {
        (true, true, _, _) => Some(EmptyRoundReason::Mixed),
        (true, false, _, true) => Some(EmptyRoundReason::RetriesExhausted),
        (true, false, _, false) => Some(EmptyRoundReason::Failed),
        (false, true, true, _) => Some(EmptyRoundReason::Mixed),
        (false, true, false, _) => Some(EmptyRoundReason::Refused),
        (false, false, true, _) => Some(EmptyRoundReason::ContentRefused),
        (false, false, false, _) => Some(EmptyRoundReason::NoAdmits),
    }
}

// ---------------------------------------------------------------------
// drb1-t5 — the support binder.
//
// Its predecessor located a claim's support with
// `chunk.content.contains(specific)`. That is brittle string matching:
// measured over the logged t7a flight, 125 of 137 claims (91%) bound to
// ZERO origins while 136 of 137 carried their own `[Source: ev-N]`
// marker — research prose paraphrases its sources, and
// `containment.rs` already conceded the class ("figureless claims merge
// nothing"). It is the same keyword-matcher failure the router replaced
// three times before (`current_info_classifier`, `scope_classifier`,
// `claim_class_classifier`).
//
// The replacement composes three stages, each asked ONLY what it can
// answer:
//
//   1. figures  — a claim's digits must appear verbatim in the chunk.
//                 A number is a feature of the claim's FORM, not its
//                 vocabulary, so code enforces it (§7.6). Honesty-
//                 critical and never delegated to a model.
//   2. locate   — embedding argmax over the chunk's spans. This is the
//                 house method for open text (§2.4, principle 9) and it
//                 replaces `contains()`. It answers only "which part of
//                 this chunk is about this claim".
//   3. decide   — the located span goes to the calibrated judge.
//                 Similarity CANNOT see negation: "affects more men
//                 than women" and "affects more women than men" are
//                 neighbours in embedding space, so a cosine threshold
//                 alone would bind a CONTRADICTING chunk as support and
//                 manufacture grounding — the exact failure the
//                 corroboration floor exists to prevent.
//
// A chunk becomes an origin only when all three agree.
// ---------------------------------------------------------------------

/// Span length for stage 2. Long enough to carry a claim's context,
/// short enough that the judge in stage 3 reads one idea.
const LOCATE_SPAN_CHARS: usize = 900;
const LOCATE_SPAN_OVERLAP: usize = 250;

/// Stage-2 floor: below this cosine, no part of the chunk is "about"
/// the claim and stage 3 is never asked.
const MIN_LOCATE_SIM: f32 = 0.35;

/// Stage-3 floor on the calibrated support probability
/// (`1.0 - claim_violation_joint`).
///
/// PROVISIONAL, and named as such: it is stricter than "does not
/// violate" on purpose — a chunk that is merely SILENT on a claim sits
/// near 0.5 and must not count as support. The calibration this owes is
/// the standing honesty banks (P4-v0, R-12), which measure both
/// directions: recovered true claims AND admitted fabrications (§18.6 —
/// a judge change reported only in the direction it was meant to fix is
/// not a measurement).
const SUPPORT_FLOOR: f64 = 0.65;

/// Split a chunk into overlapping spans for stage 2.
fn locate_spans(content: &str) -> Vec<String> {
    let t: String = super::scrub_control(content)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = t.chars().collect();
    let step = LOCATE_SPAN_CHARS.saturating_sub(LOCATE_SPAN_OVERLAP).max(1);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let end = (i + LOCATE_SPAN_CHARS).min(chars.len());
        let span: String = chars[i..end].iter().collect();
        if span.chars().count() > 150 || out.is_empty() {
            out.push(span);
        }
        if end == chars.len() {
            break;
        }
        i += step;
    }
    out
}

/// Stage 1 — every figure the claim asserts must be present verbatim.
/// Citation handles are stripped first so `[Source: ev-2]` never
/// contributes a bare "2" (the fold's own anti-leak precedent).
fn claim_figures_present(claim: &str, content: &str) -> bool {
    let stripped = super::containment::strip_citation_spans(claim);
    super::figure_tokens(&stripped)
        .iter()
        .all(|f| content.contains(f.as_str()))
}

/// How a chunk's support was located — recorded, never inferred, so a
/// degraded run is visible rather than silently scored (§18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocatedBy {
    /// Stage 2 ran: an embedded span cleared `MIN_LOCATE_SIM`.
    Embedding,
    /// The provider has no embedding surface; stage 2 could not run and
    /// the binder fell back to verbatim specifics. NAMED in the record.
    VerbatimFallback,
}

/// Stages 2+3 for one chunk. `Ok(None)` = located but unsupported;
/// `Err` = the embedding surface is unavailable (the caller degrades and
/// names it).
async fn chunk_supports(
    provider: &Arc<dyn InferenceProvider>,
    claim: &str,
    claim_vec: &[f32],
    content: &str,
    posture: ShardingPrivacy,
) -> Result<bool, String> {
    let spans = locate_spans(content);
    if spans.is_empty() {
        return Ok(false);
    }
    let vecs = provider
        .embed_batch(&spans)
        .await
        .map_err(|e| format!("span embed: {e}"))?;
    if vecs.iter().any(|v| v.is_empty()) {
        // Same rule as the claim vector: absence is reported, never
        // scored as a miss.
        return Err("zero-dimension span embedding".to_string());
    }
    let mut best: Option<(f32, usize)> = None;
    for (i, v) in vecs.iter().enumerate() {
        let sim = super::cosine(claim_vec, v);
        match best {
            Some((b, _)) if sim <= b => {}
            _ => best = Some((sim, i)),
        }
    }
    let Some((sim, idx)) = best else {
        return Ok(false);
    };
    let span = &spans[idx];
    if sim < MIN_LOCATE_SIM {
        tracing::debug!(
            target: "deep_research",
            sim, floor = MIN_LOCATE_SIM,
            "t5 binder: no span is about this claim — stage 3 not asked"
        );
        return Ok(false);
    }
    let violation =
        claim_violation_joint(provider, claim, std::slice::from_ref(span), 1, 0, posture).await;
    let Some(violation) = violation else {
        return Ok(false);
    };
    let support = 1.0 - violation;
    tracing::debug!(
        target: "deep_research",
        sim, support, floor = SUPPORT_FLOOR,
        supported = support >= SUPPORT_FLOOR,
        "t5 binder: located span judged"
    );
    Ok(support >= SUPPORT_FLOOR)
}

pub async fn assess_claim(
    provider: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[AuditChunk],
    containment: &ContainmentConfig,
    posture: ShardingPrivacy,
    tau: f64,
) -> ClaimAudit {
    // 1. Empty window → never-ran (never a pass).
    if chunks.is_empty() {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::NeverRan,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: true,
            reason: Some("no evidence retrieved for this round".to_string()),
            corroboration: None,
        };
    }

    // 2. Judge.
    let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    let prob = claim_violation_joint(provider, claim, &texts, texts.len(), 0, posture).await;
    let Some(prob) = prob else {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some("judge failed to run (claim_violation_joint returned None)".to_string()),
            corroboration: None,
        };
    };

    // 3. Failed (violation).
    if prob >= tau {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::Failed,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some(format!("judge violation_prob {prob:.3} >= tau {tau}")),
            corroboration: None,
        };
    }

    // 4. Ref-required (order deep-research-t4a, pre-registered): the
    // draft must cite the chunks it asserts against — the model's
    // honesty discretion goes to zero (it selects which chunks to
    // cite; the gate verifies the selection). A claim without a
    // citation handle refuses; a handle naming no window chunk refuses
    // (the gate cannot verify an assertion against evidence outside
    // the window). The witness then runs against the REFERENCED chunk
    // set only — a claim can only pass when its figures verify against
    // the chunks it cites. Downgrade-only: refusal paths, never a
    // verdict conversion.
    let handles = citation_handles(claim);
    if handles.is_empty() {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::RefusedNoCitationHandle,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some("ref-required: no citation handle".to_string()),
            corroboration: None,
        };
    }
    let mut referenced_ids: Vec<String> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for h in &handles {
        if let Some(c) = chunks.iter().find(|c| &c.id == h || &c.source_url == h) {
            if !referenced_ids.contains(&c.id) {
                referenced_ids.push(c.id.clone());
            }
        } else {
            unresolved.push(h.clone());
        }
    }
    if !unresolved.is_empty() {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::RefusedUnresolvableHandle,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some(format!(
                "ref-required: citation handle(s) {unresolved:?} do not name a window chunk"
            )),
            corroboration: None,
        };
    }
    let ref_texts: Vec<String> = chunks
        .iter()
        .filter(|c| referenced_ids.contains(&c.id))
        .map(|c| c.content.clone())
        .collect();
    let witness = containment_witness(provider, claim, &ref_texts, containment, posture).await;

    // 6. Custody veto (R-3): the claim's supporting chunks must not rest
    // on unknown provenance. Locate supporting chunks by specific
    // presence (C-class) when the witness ran; if every located chunk is
    // unknown, refuse.
    let witnessable_specifics: Vec<String> = witness.specifics.clone();
    let mut supporting: Vec<String> = Vec::new();
    let mut supporting_urls: Vec<String> = Vec::new();
    let mut unknown_supporting: Vec<String> = Vec::new();

    // drb1-t5: embed the claim ONCE for stage 2. A provider with no
    // embedding surface degrades to the verbatim path and the
    // degradation is NAMED, never silently scored (§18.3).
    // A ZERO-DIMENSION vector is an absence wearing the shape of a
    // value: a provider saying "I do not embed" by returning `Ok(vec![])`.
    // Scoring it as a similarity would silently convert "unavailable"
    // into "unsupported" — the substitution §18.3 forbids. It degrades,
    // and the degradation is named.
    let claim_vec = match provider.embed(claim).await {
        Ok(v) if !v.is_empty() => Some(v),
        Ok(_) => {
            tracing::warn!(
                target: "deep_research",
                "t5 binder: provider returned a zero-dimension embedding — \
                 DEGRADED to verbatim specifics (absence, not a low score)"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                target: "deep_research",
                error = %e,
                "t5 binder: no embedding surface — DEGRADED to verbatim specifics"
            );
            None
        }
    };
    let located_by = if claim_vec.is_some() {
        LocatedBy::Embedding
    } else {
        LocatedBy::VerbatimFallback
    };

    for chunk in chunks {
        // Stage 1 runs in BOTH modes and is honesty-critical: every
        // figure the claim asserts must be verbatim in this chunk. A
        // figureless claim passes this stage vacuously.
        if !claim_figures_present(claim, &chunk.content) {
            continue;
        }
        let carries = match claim_vec.as_deref() {
            Some(cv) => match chunk_supports(provider, claim, cv, &chunk.content, posture).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        target: "deep_research",
                        error = %e,
                        chunk = %chunk.id,
                        "t5 binder: stage 2/3 unavailable for this chunk — verbatim fallback, named"
                    );
                    witnessable_specifics
                        .iter()
                        .any(|s| chunk.content.contains(s))
                }
            },
            None => witnessable_specifics
                .iter()
                .any(|s| chunk.content.contains(s)),
        };
        if carries {
            if chunk.custody_known {
                supporting.push(chunk.id.clone());
                supporting_urls.push(chunk.source_url.clone());
            } else {
                unknown_supporting.push(chunk.id.clone());
            }
        }
    }
    tracing::debug!(
        target: "deep_research",
        located_by = ?located_by,
        supporting = supporting.len(),
        origins = supporting_urls.len(),
        "t5 binder: support located"
    );
    let no_known_support = supporting.is_empty() && !unknown_supporting.is_empty();
    if no_known_support {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::RefusedUnknownProvenance,
            witness: WitnessRecord {
                ran: witness.ran,
                specifics: witness.specifics,
                all_absent: witness.all_absent,
                reason: Some(format!(
                    "supporting chunks have unknown provenance: {unknown_supporting:?}"
                )),
            },
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some("refused: claim rests on unknown-provenance evidence (R-3)".to_string()),
            corroboration: None,
        };
    }

    // Witness downgrade: all witnessable specifics absent (or the
    // negative-claim rule's contradicted negation).
    if witness.ran && witness.all_absent {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord {
                ran: true,
                specifics: witness.specifics,
                all_absent: true,
                // The witness's own reason when it named one (the
                // negative-claim rule: "contradicted" vs "holds" — the
                // generic all-absent string would be a false record for
                // a contradicted negation, whose specifics ARE present);
                // the generic shape otherwise.
                reason: witness.reason.or_else(|| {
                    Some(
                        "all extracted specifics absent from the evidence (containment witness)"
                            .to_string(),
                    )
                }),
            },
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: None,
            corroboration: None,
        };
    }

    // 7. Corroboration floor (GAP-2/F22, the two-source rule): a claim
    // passes only if its support set spans ≥2 distinct provenance
    // origins. C-class: origins are the distinct source_urls among the
    // supporting chunks — coverage counts origins, never chunks (five
    // copies of one page are one origin). Downgrade-only, and the
    // record is the gate's own accounting on BOTH sides of the floor —
    // a passing claim carries `passes_floor: true`, a capped one the
    // single-origin set. An unwitnessable claim has an empty support set
    // (0 origins) and cannot pass — judge-supported is not
    // corroborated.
    const CORROBORATION_FLOOR: usize = 2;
    let mut origins = supporting_urls;
    origins.sort();
    origins.dedup();
    let passes_floor = origins.len() >= CORROBORATION_FLOOR;
    let corroboration = CorroborationRecord {
        origins: origins.clone(),
        support_chunks: supporting.len(),
        floor: CORROBORATION_FLOOR,
        passes_floor,
    };
    if !passes_floor {
        return ClaimAudit {
            claim: claim.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::CorroborationFloor,
            witness: WitnessRecord {
                ran: witness.ran,
                specifics: witness.specifics,
                all_absent: witness.all_absent,
                reason: None,
            },
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some(format!(
                "corroboration floor: {} supporting chunk(s) from {} distinct origin(s); \
                 floor is {CORROBORATION_FLOOR}",
                supporting.len(),
                origins.len()
            )),
            corroboration: Some(corroboration),
        };
    }

    // Supported + corroborated: passed, with C-class located citations.
    ClaimAudit {
        claim: claim.to_string(),
        verdict: Verdict::Passed,
        action: GateAction::CitationGrounded,
        witness: WitnessRecord {
            ran: witness.ran,
            specifics: witness.specifics,
            all_absent: witness.all_absent,
            reason: None,
        },
        supporting_chunk_ids: supporting,
        empty_evidence_window: false,
        reason: None,
        corroboration: Some(corroboration),
    }
}

/// Build a round's gap list ICD from the claim audits. Gaps are the
/// could-not-judge + never-ran claims (a failed claim is refuted by
/// evidence, not a gap). `prior_gap_texts` is the previous round's gap
/// claim texts for the strict-subset test (round 1 = baseline → true).
/// `question` supplies the empty-window gap's query: when no evidence
/// was retrieved at all, the only search-actionable phrasing is the
/// question itself — keyed structurally on `empty_evidence_window`,
/// never on the abstention text's wording (icd-schemas.md §4:
/// `actionable_query` is "the compass's output that drives R4").
/// `question_specifiers` feeds the gap-ledger fold (order
/// deep-research-t6c): the question's own figure specifiers are not
/// the claim's figures, so `gap_identity` strips them before the
/// fold's fact comparison.
pub fn build_gap_list(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    audits: &[ClaimAudit],
    prior_gap_texts: &[String],
    question: &str,
    question_specifiers: &[String],
    query_for: &dyn Fn(&str, Option<&CorroborationRecord>) -> String,
) -> GapList {
    let claims: Vec<ClaimVerdict> = audits
        .iter()
        .enumerate()
        .map(|(i, a)| ClaimVerdict {
            id: format!("c{}", i + 1),
            text: a.claim.clone(),
            verdict: a.verdict,
            evidence_ids: a.supporting_chunk_ids.clone(),
            witness: a.witness.clone(),
            action: a.action,
            empty_evidence_window: a.empty_evidence_window,
            corroboration: a.corroboration.clone(),
        })
        .collect();
    let empty_windows: Vec<EmptyWindow> = audits
        .iter()
        .enumerate()
        .filter(|(_, a)| a.empty_evidence_window)
        .map(|(i, a)| EmptyWindow {
            claim_id: format!("c{}", i + 1),
            reason: a.reason.clone().unwrap_or_default(),
        })
        .collect();
    // ---- Gap-ledger fold (order deep-research-t6c, pre-registered) ----
    // The ledger's identity is the FACT, not the sentence: a capped
    // claim whose fact is already tracked folds into the tracked
    // entry instead of adding a new gap text (the measured v1 churn:
    // the draft re-expresses already-tracked facts over the growing
    // window each round — 30/31 new r3 texts shared prior figures).
    // The prior round's texts are SEEDED first (each an entry with
    // canonical = its own text), so the canonical (first-seen) text
    // is always the prior text and the strict-subset relation holds
    // by construction when nothing new opened; a genuinely new fact
    // still enters (honest growth). An entry is EMITTED only when a
    // gap audit matched it — the prior text's own verbatim re-audit
    // (audit_pass always re-enters it) is the closing path: passing
    // the floor makes it not-a-gap, the seed stays un-emitted, and
    // the fact leaves the ledger exactly as before the fold. The
    // fold rule: figures intersect AND subjects intersect, or both
    // figureless with ≥2 shared subjects; an EMPTY identity (no
    // figures, <2 subjects) never folds — the degenerate list-fragment
    // claims stay honest entries. gap_identity in mod.rs is the ONE
    // decider; recomputed per round (stateless, no ledger to corrupt).
    struct Tracked {
        figures: Vec<String>,
        subjects: Vec<String>,
        canonical: String,
        emitted: bool,
        from_claim_id: Option<usize>,
        empty_evidence_window: bool,
        corroboration: Option<CorroborationRecord>,
    }
    let mut tracked: Vec<Tracked> = prior_gap_texts
        .iter()
        .map(|p| {
            let (figures, subjects) = super::gap_identity(p, question_specifiers);
            Tracked {
                figures,
                subjects,
                canonical: p.clone(),
                emitted: false,
                from_claim_id: None,
                empty_evidence_window: false,
                corroboration: None,
            }
        })
        .collect();
    // The fold rule, ONE decider (§10.6): figures intersect AND
    // subjects intersect, or both figureless with >= 2 shared
    // subjects; an EMPTY identity never folds. Shared by the gap pass
    // and the closure pass below.
    let folds_into =
        |tracked: &[Tracked], figures: &[String], subjects: &[String]| -> Option<usize> {
            tracked.iter().position(|t| {
                if !t.figures.is_empty() && !figures.is_empty() {
                    figures.iter().any(|f| t.figures.contains(f))
                        && subjects.iter().any(|s| t.subjects.contains(s))
                } else if t.figures.is_empty() && figures.is_empty() {
                    subjects.iter().filter(|s| t.subjects.contains(s)).count() >= 2
                } else {
                    false // one figured, one not — different facts
                }
            })
        };
    // Pass 1 (the fold, unchanged): a GAP audit that folds re-opens
    // the tracked entry — the canonical text stays the entry; THIS
    // audit's record rides the query. A gap audit that folds into
    // nothing pushes a new tracked entry (honest growth).
    for (i, a) in audits.iter().enumerate() {
        if !a.is_gap() {
            continue;
        }
        let (figures, subjects) = super::gap_identity(&a.claim, question_specifiers);
        if let Some(j) = folds_into(&tracked, &figures, &subjects) {
            tracked[j].emitted = true;
            tracked[j].from_claim_id = Some(i);
            tracked[j].empty_evidence_window = a.empty_evidence_window;
            tracked[j].corroboration = a.corroboration.clone();
            continue;
        }
        tracked.push(Tracked {
            figures,
            subjects,
            canonical: a.claim.clone(),
            emitted: true,
            from_claim_id: Some(i),
            empty_evidence_window: a.empty_evidence_window,
            corroboration: a.corroboration.clone(),
        });
    }
    // Pass 2 (T6c REV-4, pre-registered — the fold-identity closure):
    // a PASSING audit that folds into a tracked entry CLOSES it. The
    // passing claim cleared the floor with >= 2 origins, so the fact
    // identity is grounded — the ledger's identity is the fact, not
    // the sentence — and the seed's own unpassable text no longer
    // keeps the fact open. Order-independent: the passing fold wins
    // over a same-round gap fold (the grounded fact is grounded).
    // Closing the entry removes the gap AND its query — the final
    // report's coverage is untouched (the closing claim itself is
    // stateable in the round's evidence).
    for a in audits.iter() {
        if a.is_gap() {
            continue;
        }
        let (figures, subjects) = super::gap_identity(&a.claim, question_specifiers);
        if let Some(j) = folds_into(&tracked, &figures, &subjects) {
            tracked[j].emitted = false;
        }
    }
    let gaps: Vec<Gap> = tracked
        .iter()
        .filter(|t| t.emitted)
        .enumerate()
        .map(|(k, t)| Gap {
            id: format!("g{}", k + 1),
            text: t.canonical.clone(),
            actionable_query: if t.empty_evidence_window {
                question.to_string()
            } else {
                // t1d fix 3 (second-origin): the query form is chosen
                // with the corroboration record in view — a
                // floor-capped claim is queried as a FACT, not as the
                // prose cut (the query for the missing origin must
                // carry the figure the second origin must match).
                query_for(&t.canonical, t.corroboration.as_ref())
            },
            from_claim_id: t.from_claim_id.map(|i| format!("c{}", i + 1)),
            corroboration: t.corroboration.clone(),
        })
        .collect();
    let this_gap_texts: Vec<String> = gaps.iter().map(|g| g.text.clone()).collect();
    let strict_subset = if round == 1 {
        true // baseline round — nothing to shrink from
    } else {
        !this_gap_texts.is_empty()
            && this_gap_texts.len() < prior_gap_texts.len()
            && this_gap_texts
                .iter()
                .all(|t| prior_gap_texts.iter().any(|p| p == t))
    };
    GapList {
        icd: "gap_list".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        claims,
        gaps,
        empty_evidence_windows: empty_windows,
        strict_subset_of_prior: strict_subset,
    }
}

/// Read the live threshold once (the loop's audit uses the same
/// threshold the bench-calibrated judge transfers).
pub fn run_tau() -> f64 {
    grounding_gate_threshold()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Custody;
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;

    #[test]
    /// RED before `blank_out_heading_lines`. The deliverable's H1 is
    /// `# {question}`; a question ends in `?`, so the splitter emitted the
    /// user's own question as a claim and `render.rs` stamped a verdict on
    /// it. First live composed flight shipped a report titled
    /// `# Is there a general method ...? **[refuted by the evidence]**`.
    #[test]
    fn the_report_title_is_never_extracted_as_a_claim() {
        let draft = "# Is there a general method for solving asymmetric auctions?\n\n\
                     Bidders draw values independently. [Source: ev-1]\n";
        let claims = split_claims(draft);
        assert!(
            !claims.iter().any(|c| c.contains("general method")),
            "the question must not be a claim, got: {claims:?}"
        );
        assert_eq!(claims.len(), 1, "only the body sentence is a claim");
        assert!(claims[0].contains("Bidders draw values independently"));
    }

    /// RED before the fix. `compose_report` writes `### Heading` immediately
    /// followed by the first sentence with no blank line, so the heading was
    /// absorbed into that sentence and surfaced inside the Verification
    /// list as claim text.
    #[test]
    fn a_markdown_heading_is_never_swallowed_into_the_following_sentence() {
        let draft = "### Focus on Agent Interoperability Protocols\n\
                     Instead of auction mechanics, the evidence details A2A. [Source: ev-2]\n";
        let claims = split_claims(draft);
        assert_eq!(
            claims.len(),
            1,
            "one sentence, not a heading+sentence: {claims:?}"
        );
        assert!(
            !claims[0].contains("Focus on Agent Interoperability"),
            "heading leaked into the claim: {}",
            claims[0]
        );
        assert!(claims[0].contains("Instead of auction mechanics"));
    }

    /// A `#` that is not an ATX heading is prose and stays. Guards against
    /// the fix eating issue refs and the like.
    #[test]
    fn a_hash_mid_line_is_prose_not_a_heading() {
        let draft = "The regression is tracked as issue #42 in the tracker. [Source: ev-3]\n";
        let claims = split_claims(draft);
        assert_eq!(claims.len(), 1);
        assert!(claims[0].contains("issue #42"), "got: {}", claims[0]);
    }

    fn sentence_splitter_attaches_spans() {
        let draft = "The Meridian Bridge was completed in 1873 [Source: https://example.com/a]. Its span is 240 meters [Source: https://example.com/b]. A final sentence with no citation.";
        let claims = split_claims(draft);
        assert_eq!(claims.len(), 3);
        assert!(claims[0].contains("1873"));
        assert!(claims[0].contains("[Source: https://example.com/a]"));
        assert!(!claims[1].contains("1873"));
        assert!(claims[1].contains("[Source: https://example.com/b]"));
        // The final sentence inherits the paragraph's last-seen span
        // (the t6b propagation — the old exact-equality here encoded the
        // dropped-tag defect the ceiling fixture measured).
        assert!(claims[2].contains("[Source: https://example.com/b]"));
    }

    /// RED-first (order deep-research-t6b, pre-registered): the frozen
    /// ceiling task-56 draft shape — the model writes ONE terminal span
    /// per paragraph, and the splitter's attach-to-preceding-sentence
    /// rule leaves the paragraph's earlier sentences untagged, which the
    /// ref-required stage refuses ("no citation handle" — 6/23 claims on
    /// the perfect-acquisition fixture). The propagation: an untagged
    /// sentence inherits the paragraph's last-seen span — the witness
    /// still verifies each claim against the referenced chunk, so the
    /// inherited span routes the claim INTO verification, never around
    /// it.
    #[test]
    fn paragraph_span_propagates_to_untagged_sibling_sentences() {
        let draft = "Yes, there is a general method for solving first-price sealed-bid auctions with two ex-ante asymmetric bidders, but it generally requires numerical approaches rather than closed-form analytical solutions. Consequently, sophisticated numerical methods are necessary to determine the equilibrium strategies [Source: ev-1].";
        let claims = split_claims(draft);
        assert_eq!(claims.len(), 2, "two sentences: {:?}", claims);
        assert!(
            claims[0].contains("[Source: ev-1]"),
            "the untagged first sentence must inherit the paragraph's span: {:?}",
            claims[0]
        );
        assert!(claims[1].contains("[Source: ev-1]"));
    }

    /// The span is paragraph-scoped: a blank line resets it, so a later
    /// paragraph never inherits the previous paragraph's chunk.
    #[test]
    fn span_does_not_cross_paragraph_boundaries() {
        let draft = "A claim resting on chunk one [Source: ev-1].\n\nA new paragraph with its own evidence [Source: ev-2]. A sibling sentence.";
        let claims = split_claims(draft);
        assert_eq!(claims.len(), 3, "{:?}", claims);
        assert!(claims[0].contains("[Source: ev-1]"));
        assert!(claims[1].contains("[Source: ev-2]"));
        assert!(
            claims[2].contains("[Source: ev-2]"),
            "the sibling inherits its OWN paragraph's span: {:?}",
            claims[2]
        );
        assert!(
            !claims[2].contains("ev-1"),
            "never the previous paragraph's span"
        );
    }

    #[test]
    fn empty_window_is_never_ran() {
        let audits = Vec::new();
        let gaps = build_gap_list("r", "h", 1, &audits, &[], "question?", &[], &|_, _| {
            "q".to_string()
        });
        assert!(gaps.gaps.is_empty());
        assert!(gaps.strict_subset_of_prior);
    }

    /// The empty-window gap's query is the QUESTION, not the abstention
    /// text — the compass's output drives R4 (icd-schemas.md §4).
    /// Watched failure: the demo run's first measurement showed the
    /// empty-estate abstention producing a gap whose query was the
    /// abstention text itself, unusable as a web search.
    #[test]
    fn empty_window_gap_queries_the_question() {
        let mk = |empty: bool| ClaimAudit {
            claim: "No evidence was retrieved this round.".to_string(),
            verdict: Verdict::NeverRan,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: empty,
            reason: Some("no evidence retrieved for this round".to_string()),
            corroboration: None,
        };
        let g = build_gap_list(
            "r",
            "h",
            1,
            &[mk(true)],
            &[],
            "What is the question?",
            &[],
            &|c, _| format!("TEMPLATED:{c}"),
        );
        assert_eq!(g.gaps.len(), 1);
        assert_eq!(
            g.gaps[0].actionable_query, "What is the question?",
            "an empty-window gap must query the question, never the abstention text"
        );
        // A claim-shaped gap keeps the deterministic template.
        let g = build_gap_list(
            "r",
            "h",
            1,
            &[mk(false)],
            &[],
            "What is the question?",
            &[],
            &|c, _| format!("TEMPLATED:{c}"),
        );
        assert_eq!(
            g.gaps[0].actionable_query,
            "TEMPLATED:No evidence was retrieved this round."
        );
    }

    #[test]
    fn strict_subset_is_computed() {
        let mk = |text: &str| ClaimAudit {
            claim: text.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::AbstainedDecline,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: None,
            corroboration: None,
        };
        // Round 2 with gaps ⊆ round 1's → strict subset when smaller.
        let prior = vec!["a".to_string(), "b".to_string()];
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk("a")],
            &prior,
            "question?",
            &[],
            &|_, _| "q".to_string(),
        );
        assert!(g.strict_subset_of_prior);
        assert_eq!(g.gaps.len(), 1);
        // Same size → not strict.
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk("a"), mk("b")],
            &prior,
            "question?",
            &[],
            &|_, _| "q".to_string(),
        );
        assert!(!g.strict_subset_of_prior);
        // A new gap (not in prior) → not a subset.
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk("c")],
            &prior,
            "question?",
            &[],
            &|_, _| "q".to_string(),
        );
        assert!(!g.strict_subset_of_prior);
    }

    // ---- Gap-ledger fold (order deep-research-t6c, pre-registered):
    // the open-question control order. A capped claim whose FACT is
    // already tracked folds into the prior entry instead of entering
    // the ledger as a new text — the ledger's identity is the one
    // decider `gap_identity` (figures minus the question's specifiers,
    // plus subject terms); the canonical text is the first-seen one.
    // The prior gap's own re-audit (verbatim text) still carries the
    // closing path; a genuinely-new fact still enters (honest growth).
    // The audit's claims array keeps every capped claim — only the
    // ledger dedupes by fact. ----

    fn mk_gap(text: &str) -> ClaimAudit {
        ClaimAudit {
            claim: text.to_string(),
            verdict: Verdict::CouldNotJudge,
            action: GateAction::CorroborationFloor,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: Some("corroboration floor".to_string()),
            corroboration: None,
        }
    }

    /// A passing audit: the floor cleared (>= 2 origins) — the
    /// REV-4 closure's subject shape (the real passed claims read
    /// `Verdict::Passed` / `GateAction::CitationGrounded`).
    fn mk_pass(text: &str) -> ClaimAudit {
        ClaimAudit {
            claim: text.to_string(),
            verdict: Verdict::Passed,
            action: GateAction::CitationGrounded,
            witness: WitnessRecord::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: None,
            corroboration: Some(CorroborationRecord {
                origins: vec![
                    "https://ev-1.example".to_string(),
                    "https://ev-2.example".to_string(),
                ],
                support_chunks: 2,
                floor: 2,
                passes_floor: true,
            }),
        }
    }

    /// RED (order deep-research-t6c): the measured v1 churn — the same
    /// fact re-stated with new wording and figure accretion
    /// ("Gentrification accelerated significantly after 2000…" gains
    /// "20%…9%" specifics) enters the ledger as a NEW text at HEAD,
    /// so the gap set grows 39 → 66. The fold must absorb the
    /// re-statement into the tracked entry and keep the canonical text.
    #[test]
    fn rephrased_gap_folds_into_prior_entry() {
        let prior = vec![
            "Gentrification accelerated significantly after 2000, with rates doubling compared \
             to the 1990s [Source: ev-1]."
                .to_string(),
        ];
        let restated = "Gentrification accelerated significantly after 2000, with rates doubling \
                        compared to the 1990s; specifically, nearly 20% of lower-income \
                        neighborhoods experienced gentrification since 2000 [Source: ev-1]."
            .to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&restated)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(
            g.gaps.len(),
            1,
            "the re-stated fact must fold into the tracked gap, not add a new text: {:?}",
            g.gaps.iter().map(|x| &x.text).collect::<Vec<_>>()
        );
        assert_eq!(
            g.gaps[0].text, prior[0],
            "the canonical (first-seen) text is kept verbatim"
        );
    }

    /// A genuinely-new fact (a figure set never tracked) still enters —
    /// honest growth is preserved, and the round cannot be a strict
    /// subset when a new question opened. The fixture mirrors
    /// audit_pass: the prior text's own verbatim re-audit is present
    /// (it folds into its seeded entry, keeping the fact tracked).
    #[test]
    fn genuinely_new_fact_still_enters_the_ledger() {
        let prior =
            vec!["Gentrification accelerated significantly after 2000 [Source: ev-1].".to_string()];
        let fresh = "In terms of raw totals, the highest number of tracts (128) gentrified in \
                     New York [Source: ev-1]."
            .to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_gap(&fresh)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 2, "a new fact is a new gap: {:?}", g.gaps);
        assert_eq!(g.gaps[0].text, prior[0], "the prior fact stays tracked");
        assert_eq!(g.gaps[1].text, fresh);
        assert!(!g.strict_subset_of_prior);
    }

    /// A figureless re-statement folds by shared subjects (≥2) — the
    /// measured "Regional patterns show that Pacific Northwest cities…"
    /// shape.
    #[test]
    fn figureless_restatement_folds_by_subjects() {
        let prior = vec![
            "Regional patterns show that Pacific Northwest cities exhibited intensive \
             transformation patterns most frequently [Source: ev-1]."
                .to_string(),
        ];
        let restated = "Regional patterns show that Pacific Northwest cities and Northeast \
                        Corridor metros exhibited these intensive transformation patterns most \
                        frequently [Source: ev-1]."
            .to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&restated)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 1);
        assert_eq!(g.gaps[0].text, prior[0]);
    }

    /// Different facts sharing a subject never fold on the subject
    /// alone — Gini 0.5469 is not Gini 0.40 (the scorer's own
    /// figure-identity discipline, mirrored). The prior text's
    /// verbatim re-audit keeps it tracked.
    #[test]
    fn different_figure_same_subject_does_not_fold() {
        let prior = vec!["Gini coefficient reached 0.40 by 2013 [Source: ev-1].".to_string()];
        let other = "Gini coefficient reached 0.5469 in New York City [Source: ev-1].".to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_gap(&other)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 2, "disjoint figures are disjoint facts");
        assert_eq!(g.gaps[1].text, other, "the new fact is its own gap");
    }

    // --- REV-4 (order deep-research-t6c, pre-registered): the
    // fold-identity closure. RED: the fold loop `continue`s on
    // !is_gap() at HEAD — a passing claim's fold relation is never
    // evaluated, so the tests below fail at HEAD and pass after the
    // two-pass fold (watched red, then green).

    /// A passing (non-gap) audit with the same fact identity closes
    /// the seed — the passing claim cleared the floor with >= 2
    /// origins, so the fact is grounded even though the seed's own
    /// text is unpassable. RED at HEAD: the seed's own gap re-audit
    /// folds and keeps it emitted; the passing fold is skipped.
    #[test]
    fn passing_fold_closes_the_seed() {
        let prior = vec![
            "Gini coefficient reached 0.40 by 2013, marking a steady widening of income \
             inequality [Source: ev-1]."
                .to_string(),
        ];
        let passing =
            "Gini coefficient reached 0.40 by 2013 [Source: ev-1] [Source: ev-2].".to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_pass(&passing)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert!(
            g.gaps.is_empty(),
            "a passing fold proves the fact grounded — the seed must close: {:?}",
            g.gaps.iter().map(|x| &x.text).collect::<Vec<_>>()
        );
    }

    /// The closing pass never closes a seed on a passing claim with a
    /// DIFFERENT figure — Gini 0.5469 does not ground Gini 0.40 (the
    /// fold rule is unchanged; the same discipline as the scorer's
    /// figure identity).
    #[test]
    fn passing_different_figure_does_not_close_the_seed() {
        let prior = vec!["Gini coefficient reached 0.40 by 2013 [Source: ev-1].".to_string()];
        let passing =
            "Gini coefficient reached 0.5469 in New York City [Source: ev-1] [Source: ev-2]."
                .to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_pass(&passing)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 1, "disjoint figures are disjoint facts");
    }

    /// Empty-identity entries (the degenerate list-fragment class)
    /// never fold and never close — a passing claim about a different,
    /// figured fact leaves the abstention tracked.
    #[test]
    fn empty_identity_seed_is_unaffected_by_passing_folds() {
        let prior = vec!["No evidence was retrieved this round.".to_string()];
        let passing = "Portland (58.1% of eligible tracts) led gentrification [Source: ev-1] \
             [Source: ev-2]."
            .to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_pass(&passing)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 1, "the abstention stays tracked");
    }

    /// The round-1 empty-window abstention gap never absorbs content
    /// gaps — the r1→r2 transition stays honest (39 ⊄ {abstention}).
    /// The abstention's own verbatim re-audit keeps it tracked; the
    /// content gap (figured, so never matching the figureless
    /// abstention identity) enters as its own entry.
    #[test]
    fn abstention_gap_does_not_absorb_content_gaps() {
        let prior = vec!["No evidence was retrieved this round.".to_string()];
        let content =
            "Portland (58.1% of eligible tracts) led gentrification [Source: ev-1].".to_string();
        let g = build_gap_list(
            "r",
            "h",
            2,
            &[mk_gap(&prior[0]), mk_gap(&content)],
            &prior,
            "q?",
            &[],
            &|_, _| "query".to_string(),
        );
        assert_eq!(g.gaps.len(), 2);
        assert_eq!(
            g.gaps[0].text, prior[0],
            "the abstention stays its own entry"
        );
        assert!(!g.strict_subset_of_prior);
    }

    // ---- Witness-fix (directive 6c25d88e): the negative-claim rule's
    // reason must flow through the audit record (the generic
    // "all extracted specifics absent" string would be a false record
    // for a contradicted negation — the specifics ARE present). ----

    /// Shape-keyed scripted provider: judge calls (structured_output
    /// Some) answer the forced-choice A/B JSON; every other call (the
    /// witness's extraction) answers the scripted text. The joint judge
    /// makes exactly one provider call, so the audit path is fully
    /// deterministic.
    struct ShapeScripted {
        extract: &'static str,
    }

    #[async_trait]
    impl InferenceProvider for ShapeScripted {
        async fn complete(
            &self,
            r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<crate::types::CompletionResponse> {
            let text = if r.structured_output.is_some() {
                r#"{"A": 1.0, "B": 0.0}"#.to_string()
            } else {
                self.extract.to_string()
            };
            Ok(crate::types::CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "test".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<Pin<Box<dyn Stream<Item = crate::error::Result<String>> + Send>>>
        {
            unimplemented!()
        }
        async fn embed(&self, _t: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![])
        }
        fn capabilities(&self) -> crate::types::ProviderCapabilities {
            crate::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            }
        }
    }

    fn apollo_window() -> Vec<AuditChunk> {
        vec![AuditChunk {
            id: "c1".to_string(),
            content: concat!(
                "The Apollo 11 mission launched on July 16, 1969, and its crew of three ",
                "— Neil Armstrong, Buzz Aldrin, and Michael Collins — landed on the Moon on July 20."
            )
            .to_string(),
            custody_known: true,
            source_url: "https://example.com/apollo".to_string(),
        }]
    }

    #[tokio::test]
    async fn contradicted_negative_records_its_reason_in_the_audit() {
        // Ref-required amendment (order deep-research-t4a,
        // pre-registered): the fixture claim gains its citation handle
        // (the apollo_window chunk id).
        let claim =
            "None of the provided sources list the crew members of the Apollo 11 mission. [Source: c1]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted {
            extract: "Apollo 11",
        });
        let audit = assess_claim(
            &provider,
            claim,
            &apollo_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(audit.verdict, Verdict::CouldNotJudge);
        assert!(audit.witness.ran && audit.witness.all_absent);
        assert!(
            audit
                .witness
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("negative")),
            "the contradicted negation must record ITS reason, not the generic all-absent string"
        );
    }

    #[tokio::test]
    async fn vacuous_negative_is_could_not_judge_not_passed() {
        // Ref-required amendment (order deep-research-t4a,
        // pre-registered): the fixture claim gains its citation handle
        // (the apollo_window chunk id).
        let claim =
            "None of the provided sources list the crew members of the Apollo 11 mission. [Source: c1]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted { extract: "NONE" });
        let audit = assess_claim(
            &provider,
            claim,
            &apollo_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(audit.verdict, Verdict::CouldNotJudge);
        assert!(
            audit.witness.ran && audit.witness.all_absent,
            "an unverifiable negative claim is never a vacuous pass"
        );
    }

    // ---- Claim-figure honesty (order deep-research-t1h,
    // pre-registered): the t1g partial-trace red — the probe's final
    // c1 [passed] with the untraced figure "2024": the extractor
    // dropped it from the specifics (["1980","2000","University of
    // Georgia"]) while the claim itself carried it in "(1980–2024)"
    // and the window did not (probe dr-1786928663 verdict-set.json
    // c1, gap-list-2.json, evidence-window-1.json). The claim's OWN
    // figure tokens are checked against the evidence BEFORE
    // extraction — a claim figure absent from the evidence is
    // untraced, full stop, both polarities. Downgrade-only. ----

    /// The t1g c1 era window — the probe's shape: chunks carry "since
    /// 1980," and "after 2000" but NOT "2024"; TWO distinct origins so
    /// the corroboration floor passes and the witness is the only gate
    /// that can cap (probe evidence-window-1.json: fetch ev-1..3 +
    /// estate chunks 21/29/33/4/50/64 — "1980" in chunk 50, "2000"
    /// present, "2024" absent).
    fn era_window() -> Vec<AuditChunk> {
        vec![
            AuditChunk {
                id: "c1".to_string(),
                content: concat!(
                    "American cities have experienced a fundamental transformation since 1980, ",
                    "with gentrification accelerating after 2000 across the nation's largest urban centers."
                )
                .to_string(),
                custody_known: true,
                source_url: "https://example.com/era-one".to_string(),
            },
            AuditChunk {
                id: "c2".to_string(),
                content: concat!(
                    "Research at the University of Georgia tracks demographic shifts in American ",
                    "cities after 2000, building on patterns that emerged since 1980."
                )
                .to_string(),
                custody_known: true,
                source_url: "https://example.com/era-two".to_string(),
            },
        ]
    }

    /// RED: the probe c1 shape — a claim figure ("2024") absent from
    /// the evidence caps at could-not-judge, never passed, and the
    /// reason names the figure. The extraction never runs: the
    /// short-circuit is deterministic and extraction-independent.
    #[tokio::test]
    async fn untraced_claim_figure_is_downgraded_not_passed() {
        // Ref-required amendment (order deep-research-t4a,
        // pre-registered): the fixture's tail becomes a resolvable
        // chunk handle (era_window c2 — which lacks "2024", the
        // untraced figure).
        let claim = concat!(
            "American cities underwent dramatic economic and demographic transformations ",
            "across four decades (1980–2024), with gentrification accelerating significantly after 2000 ",
            "[Source: c2]."
        );
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted {
            extract: "1980\nUniversity of Georgia",
        });
        let audit = assess_claim(
            &provider,
            claim,
            &era_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "a claim figure ('2024') absent from the evidence must cap at could-not-judge, got {:?}",
            audit.verdict
        );
        assert!(
            audit.witness.ran && audit.witness.all_absent,
            "the witness runs and reports the untraced figure"
        );
        assert!(
            audit
                .witness
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("2024")),
            "the reason must name the untraced figure, got {:?}",
            audit.witness.reason
        );
    }

    /// Positive control: when every claim figure IS present in the
    /// evidence, the witness is NOT blocked — the strengthen only ever
    /// adds downgrades, never removes true positives.
    #[tokio::test]
    async fn fully_traced_claim_figures_do_not_block_the_witness() {
        // Ref-required amendment (order deep-research-t4a,
        // pre-registered): the fixture claim gains its citation handle
        // (era_window c1 — the figures it asserts are present there).
        let claim = concat!(
            "American cities have been transformed by gentrification since 2000, ",
            "with governing coalitions reshaping urban policy across the nation. [Source: c1]"
        );
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted {
            extract: "2000\nGoverning",
        });
        let audit = assess_claim(
            &provider,
            claim,
            &era_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::Passed,
            "fully traced claim figures do not block the witness, got {:?}",
            audit.verdict
        );
        assert_eq!(audit.action, GateAction::CitationGrounded);
    }

    /// The negative shape: a negative claim whose figures are absent
    /// from the evidence is UNVERIFIABLE — the short-circuit covers
    /// both polarities (absence-of-the-figure is consistent with the
    /// negation but cannot verify it); downgraded, never passed.
    #[tokio::test]
    async fn negative_claim_with_untraced_figures_is_downgraded_not_passed() {
        // Ref-required amendment (order deep-research-t4a,
        // pre-registered): the fixture claim gains its citation handle
        // (era_window c2 — which lacks "2024").
        let claim = "No source lists the 2024 census figures for the transformation of American cities. [Source: c2]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted { extract: "NONE" });
        let audit = assess_claim(
            &provider,
            claim,
            &era_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "a negative claim with an untraced figure ('2024') is unverifiable — never a pass, got {:?}",
            audit.verdict
        );
        assert!(
            audit
                .witness
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("2024")),
            "the reason must name the untraced figure, got {:?}",
            audit.witness.reason
        );
    }

    // ------------------------------------------------------------------
    // GAP-2 — the corroboration floor (F22, the two-source rule).
    // RED-FIRST: single-origin support passes today; the floor must cap
    // it at could-not-judge. The two-origin twin guards the
    // downgrade-only invariant (a claim the floor lets through passes
    // exactly as it did before).
    // ------------------------------------------------------------------

    /// A two-chunk window for the floor tests — the scripted specific
    /// ("Apollo 11") is present in every chunk, so every chunk carries
    /// support; only the ORIGIN SET differs between the twins.
    fn two_origin_window(origins: &[&str]) -> Vec<AuditChunk> {
        origins
            .iter()
            .enumerate()
            .map(|(i, url)| AuditChunk {
                id: format!("c{}", i + 1),
                content: concat!(
                    "The Apollo 11 mission launched on July 16, 1969, and its crew of three ",
                    "— Neil Armstrong, Buzz Aldrin, and Michael Collins — landed on the Moon on July 20."
                )
                .to_string(),
                custody_known: true,
                source_url: url.to_string(),
            })
            .collect()
    }

    /// F22's exact shape: TWO chunks from ONE document look corroborated
    /// when coverage counts chunks — the floor counts DISTINCT ORIGINS,
    /// and a one-origin support set caps at could-not-judge with the
    /// floor's record + action on the audit.
    #[tokio::test]
    async fn single_origin_support_caps_at_could_not_judge() {
        let chunks = two_origin_window(&["https://example.com/one", "https://example.com/one"]);
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted {
            extract: "Apollo 11",
        });
        let audit = assess_claim(
            &provider,
            // Ref-required amendment (order deep-research-t4a,
            // pre-registered): the fixture claim gains its citation
            // handle (two_origin_window c1).
            "The Apollo 11 mission launched on July 16, 1969. [Source: c1]",
            &chunks,
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "a single-origin support set must cap at could-not-judge, got {:?}",
            audit.verdict
        );
        assert_eq!(
            audit.action,
            GateAction::CorroborationFloor,
            "the cap must carry the floor's action"
        );
        let rec = audit
            .corroboration
            .expect("the floor's record must be on the audit");
        assert!(!rec.passes_floor);
        assert_eq!(rec.floor, 2);
        assert_eq!(rec.origins, vec!["https://example.com/one".to_string()]);
        assert_eq!(
            rec.support_chunks, 2,
            "the record counts the chunks AND the origins — never the chunks only"
        );
        assert!(
            audit.supporting_chunk_ids.is_empty(),
            "a capped claim carries no citations"
        );
    }

    /// The floor is downgrade-only: two chunks from TWO documents pass
    /// unchanged — the corroboration record with `passes_floor: true` is
    /// added, the verdict is not disturbed.
    #[tokio::test]
    async fn two_distinct_origins_pass_unchanged() {
        let chunks = two_origin_window(&["https://example.com/one", "https://example.com/two"]);
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted {
            extract: "Apollo 11",
        });
        let audit = assess_claim(
            &provider,
            // Ref-required amendment (order deep-research-t4a,
            // pre-registered): the fixture claim gains its citation
            // handle (two_origin_window c1).
            "The Apollo 11 mission launched on July 16, 1969. [Source: c1]",
            &chunks,
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(audit.verdict, Verdict::Passed, "two distinct origins pass");
        assert_eq!(audit.action, GateAction::CitationGrounded);
        let rec = audit
            .corroboration
            .expect("a passing claim carries the floor's record too");
        assert!(rec.passes_floor, "the record is the gate's own answer");
        assert_eq!(rec.origins.len(), 2);
        assert_eq!(
            audit.supporting_chunk_ids.len(),
            2,
            "both chunks carry citations"
        );
    }

    // ------------------------------------------------------------------
    // REF-REQUIRED (order deep-research-t4a, pre-registered): the
    // model's honesty discretion goes to zero — it selects which chunks
    // to cite; the gate verifies the selection. The containment witness
    // runs against the REFERENCED chunk set. RED-FIRST at HEAD: the
    // gate verifies against a paraphrase (the window), so these shapes
    // pass or cap for other reasons.
    // ------------------------------------------------------------------

    /// A two-chunk window for the ref-required reds — ev-1 carries NO
    /// figure, ev-2 carries "68"; the claim cites ev-1.
    fn ref_window() -> Vec<AuditChunk> {
        vec![
            AuditChunk {
                id: "ev-1".to_string(),
                content: "The auction house expanded its operations across the region.".to_string(),
                custody_known: true,
                source_url: "https://example.com/one".to_string(),
            },
            AuditChunk {
                id: "ev-2".to_string(),
                content: "The auction house served 68 languages worldwide across its halls."
                    .to_string(),
                custody_known: true,
                source_url: "https://example.com/one".to_string(),
            },
        ]
    }

    /// RED (order deep-research-t4a): a claim with no citation handle
    /// refuses — the draft must select the chunks it asserts against.
    #[tokio::test]
    async fn ref_required_no_handle_refuses() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted { extract: "68" });
        let audit = assess_claim(
            &provider,
            "The auction house served 68 languages worldwide.",
            &ref_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "a handle-less claim must refuse, got {:?}",
            audit.verdict
        );
        assert_eq!(
            audit.action,
            GateAction::RefusedNoCitationHandle,
            "the refusal must carry its own action, got {:?}",
            audit.action
        );
        assert!(
            audit
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("ref-required")),
            "the reason must name the ref-required class, got {:?}",
            audit.reason
        );
    }

    /// RED (order deep-research-t4a): a handle naming no window chunk
    /// refuses — the gate cannot verify an assertion against evidence
    /// outside the window.
    #[tokio::test]
    async fn ref_required_unresolvable_handle_refuses() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted { extract: "68" });
        let audit = assess_claim(
            &provider,
            "The auction house served 68 languages worldwide [Source: ev-99].",
            &ref_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "an unresolvable handle must refuse, got {:?}",
            audit.verdict
        );
        assert_eq!(
            audit.action,
            GateAction::RefusedUnresolvableHandle,
            "the refusal must carry its own action, got {:?}",
            audit.action
        );
        assert!(
            audit.reason.as_deref().is_some_and(|r| r.contains("ev-99")),
            "the reason must name the unresolvable handle, got {:?}",
            audit.reason
        );
    }

    /// RED (order deep-research-t4a — the pinned shape): a claim whose
    /// HANDLE'S chunk lacks the figure refuses (the witness fires
    /// against the referenced chunk). The figure IS in the window
    /// (ev-2) — at HEAD the window-wide witness sees it and the claim
    /// caps at the floor instead; after the fix the witness is
    /// ref-scoped and the claim's own selection fails it.
    #[tokio::test]
    async fn ref_required_claim_whose_chunk_lacks_the_figure_refuses() {
        let provider: Arc<dyn InferenceProvider> = Arc::new(ShapeScripted { extract: "68" });
        let audit = assess_claim(
            &provider,
            "The auction house served 68 languages worldwide [Source: ev-1].",
            &ref_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "a claim whose referenced chunk lacks its figure must refuse, got {:?}",
            audit.verdict
        );
        assert!(
            audit.witness.ran && audit.witness.all_absent,
            "the witness fires against the referenced chunk and reports the absence"
        );
        assert!(
            audit
                .witness
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("68")),
            "the reason must name the untraced figure, got {:?}",
            audit.witness.reason
        );
        assert_eq!(
            audit.action,
            GateAction::AbstainedDecline,
            "the ref-scoped witness downgrade keeps the abstained action, got {:?}",
            audit.action
        );
    }

    // ------------------------------------------------------------------
    // T7b — the one decider over the round window's own fields
    // (RED-FIRST: `empty_round_reason` does not exist at HEAD — this
    // test did not compile before the fix landed; order
    // deep-research-t7b, pre-registered). The four empty shapes are a
    // closed enum (§2, §10.6): refused / failed / mixed / no-admits.
    // ------------------------------------------------------------------

    fn empty_window(
        round: u32,
        fetch_failures: Vec<FetchFailure>,
        dedup_refused: Vec<String>,
        content_refused: Vec<crate::deep_research::icd::ContentRefusal>,
    ) -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "run-1".to_string(),
            charter_hash: "hash".to_string(),
            round,
            chunks: Vec::new(),
            fetch_failures,
            dedup_refused,
            content_refused,
            derived_custody: "public-web".to_string(),
        }
    }

    #[test]
    fn empty_round_reason_classifies_round_windows() {
        // Refused: everything the round admitted was already fetched
        // (the pinned t6c shape — chunks [], failures [], refused [url]).
        let refused = empty_window(
            2,
            Vec::new(),
            vec!["https://estate.example/seed-02".to_string()],
            Vec::new(),
        );
        assert_eq!(
            empty_round_reason(&refused),
            Some(EmptyRoundReason::Refused)
        );

        // Failed: admitted fetches errored, nothing refused.
        let failed = empty_window(
            2,
            vec![FetchFailure {
                url: "https://example.com/a".to_string(),
                error: "fetch failed".to_string(),
                absent: false,
                retries: 0,
                health: crate::deep_research::icd::UrlHealth::Dead,
            }],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(empty_round_reason(&failed), Some(EmptyRoundReason::Failed));

        // RetriesExhausted: fetch failed after exhausting retries.
        let retries_exhausted = empty_window(
            2,
            vec![FetchFailure {
                url: "https://example.com/b".to_string(),
                error: "fetch failed after retries".to_string(),
                absent: false,
                retries: 2,
                health: crate::deep_research::icd::UrlHealth::Dead,
            }],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            empty_round_reason(&retries_exhausted),
            Some(EmptyRoundReason::RetriesExhausted)
        );

        // Mixed: some refused, some failed.
        let mixed = empty_window(
            3,
            vec![FetchFailure {
                url: "https://example.com/b".to_string(),
                error: "fetch failed".to_string(),
                absent: false,
                retries: 0,
                health: crate::deep_research::icd::UrlHealth::Dead,
            }],
            vec!["https://estate.example/seed-02".to_string()],
            Vec::new(),
        );
        assert_eq!(empty_round_reason(&mixed), Some(EmptyRoundReason::Mixed));

        // drb1-t2 ContentRefused: pages WERE fetched but every one was
        // content-refused at the post-fetch gate — budget spent, no
        // evidence; its own named shape, not NoAdmits and not Failed.
        let content_refused = empty_window(
            3,
            Vec::new(),
            Vec::new(),
            vec![crate::deep_research::icd::ContentRefusal {
                url: "https://example.com/chrome".to_string(),
                title: "A Listing Page".to_string(),
                coverage: 0.083,
                prose_line: 42,
                reason: "content-below-threshold".to_string(),
            }],
        );
        assert_eq!(
            empty_round_reason(&content_refused),
            Some(EmptyRoundReason::ContentRefused)
        );

        // NoAdmits: nothing was admitted to fetch at all.
        let no_admits = empty_window(3, Vec::new(), Vec::new(), Vec::new());
        assert_eq!(
            empty_round_reason(&no_admits),
            Some(EmptyRoundReason::NoAdmits)
        );

        // A round that ADDED evidence is not an empty round.
        let mut populated = empty_window(1, Vec::new(), Vec::new(), Vec::new());
        populated.chunks.push(super::super::icd::WindowChunk {
            id: "ev-1".to_string(),
            locator: "l".to_string(),
            source_url: "https://example.com/a".to_string(),
            custody: Custody::PublicWeb.to_string(),
            provenance_class: "direct".to_string(),
            content: "evidence".to_string(),
            ingested_into: None,
            tags: Vec::new(),
        });
        assert_eq!(empty_round_reason(&populated), None);
    }

    // ---- drb1-t5: the three-stage binder ---------------------------

    /// A provider that DOES embed (so stage 2 runs and locates a span)
    /// and whose forced choice is scripted, so stage 3's decision is the
    /// only variable. `a`/`b` are the support/against sides of the
    /// calibrated A/B: support = a/(a+b).
    struct PolarityScripted {
        extract: &'static str,
        a: f64,
        b: f64,
    }

    #[async_trait]
    impl InferenceProvider for PolarityScripted {
        async fn complete(
            &self,
            r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<crate::types::CompletionResponse> {
            let text = if r.structured_output.is_some() {
                format!(r#"{{"A": {}, "B": {}}}"#, self.a, self.b)
            } else {
                self.extract.to_string()
            };
            Ok(crate::types::CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "test".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &crate::types::CompletionRequest,
        ) -> crate::error::Result<Pin<Box<dyn Stream<Item = crate::error::Result<String>> + Send>>>
        {
            unimplemented!()
        }
        /// A REAL vector — stage 2 can run. Every text embeds
        /// identically, so cosine is 1.0 and the span always locates:
        /// the test isolates stage 3.
        async fn embed(&self, _t: &str) -> crate::error::Result<Vec<f32>> {
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }
        fn capabilities(&self) -> crate::types::ProviderCapabilities {
            crate::types::ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            }
        }
    }

    /// THE honesty red for T5. Similarity cannot see negation: a chunk
    /// that CONTRADICTS the claim sits right next to it in embedding
    /// space, so stage 2 will happily locate a span. Stage 3 is what
    /// stops it from becoming an origin — without it the binder would
    /// manufacture the grounding the corroboration floor exists to
    /// prevent.
    #[tokio::test]
    async fn a_contradicting_chunk_never_binds_as_support() {
        let claim = "The Apollo 11 crew landed on the Moon. [Source: c1]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(PolarityScripted {
            extract: "Apollo 11",
            a: 0.0,
            b: 1.0, // the judge says: not supported
        });
        let audit = assess_claim(
            &provider,
            claim,
            &apollo_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_ne!(
            audit.verdict,
            Verdict::Passed,
            "a located-but-unsupported span must not pass the claim"
        );
        let origins = audit
            .corroboration
            .as_ref()
            .map(|c| c.origins.len())
            .unwrap_or(0);
        assert_eq!(
            origins, 0,
            "stage 3 refused, so the chunk contributes NO origin (got {origins})"
        );
    }

    /// The mirror: the same located span, the same claim, but the judge
    /// says supported — the chunk becomes an origin. Without this the
    /// test above would pass for the wrong reason (a binder that never
    /// binds anything).
    #[tokio::test]
    async fn a_supported_located_span_does_bind_as_an_origin() {
        let claim = "The Apollo 11 crew landed on the Moon. [Source: c1]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(PolarityScripted {
            extract: "Apollo 11",
            a: 1.0,
            b: 0.0, // the judge says: supported
        });
        let audit = assess_claim(
            &provider,
            claim,
            &apollo_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        let origins = audit
            .corroboration
            .as_ref()
            .map(|c| c.origins.len())
            .unwrap_or(0);
        assert_eq!(
            origins, 1,
            "the supported span binds its chunk as exactly one origin"
        );
        assert_eq!(
            audit.verdict,
            Verdict::CouldNotJudge,
            "and ONE origin still caps at the corroboration floor — the floor is untouched by T5"
        );
    }

    /// Stage 1 is honesty-critical and runs before any model call: a
    /// claim asserting a figure the chunk does not carry cannot bind to
    /// it, however similar the prose looks and however agreeable the
    /// judge is.
    #[tokio::test]
    async fn a_figure_absent_from_the_chunk_blocks_the_bind() {
        let claim =
            "The Apollo 11 mission launched on July 16, 1969 with 7 astronauts. [Source: c1]";
        let provider: Arc<dyn InferenceProvider> = Arc::new(PolarityScripted {
            extract: "7 astronauts",
            a: 1.0,
            b: 0.0, // even with the judge saying yes
        });
        let audit = assess_claim(
            &provider,
            claim,
            &apollo_window(),
            &ContainmentConfig::default(),
            ShardingPrivacy::LocalOnly,
            0.9,
        )
        .await;
        assert_ne!(
            audit.verdict,
            Verdict::Passed,
            "a claim asserting a figure absent from the evidence must never pass"
        );
    }
}
