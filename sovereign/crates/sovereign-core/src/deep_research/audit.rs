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
    ClaimVerdict, CorroborationRecord, EmptyWindow, Gap, GapList, GateAction, Verdict,
    WitnessRecord,
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
pub fn split_claims(draft: &str) -> Vec<String> {
    let mut claims = Vec::new();
    let mut current = String::new();
    // Iterate char-wise, splitting on sentence-final punctuation
    // (., !, ?) followed by whitespace or end.
    let chars: Vec<char> = draft.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        current.push(c);
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
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                claims.push(trimmed.to_string());
            }
            current.clear();
        } else {
            i += 1;
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        claims.push(tail.to_string());
    }
    claims
}

/// The composed gate over one claim. `tau` is read at run start from
/// `grounding_gate_threshold()` and frozen into the charter hash — the
/// loop re-reads nothing mid-run (FR-3).
#[allow(clippy::too_many_arguments)]
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
    for chunk in chunks {
        let carries = witnessable_specifics
            .iter()
            .any(|s| chunk.content.contains(s));
        if carries {
            if chunk.custody_known {
                supporting.push(chunk.id.clone());
                supporting_urls.push(chunk.source_url.clone());
            } else {
                unknown_supporting.push(chunk.id.clone());
            }
        }
    }
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
pub fn build_gap_list(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    audits: &[ClaimAudit],
    prior_gap_texts: &[String],
    question: &str,
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
    let gaps: Vec<Gap> = audits
        .iter()
        .enumerate()
        .filter(|(_, a)| a.is_gap())
        .map(|(i, a)| Gap {
            id: format!("g{}", i + 1),
            text: a.claim.clone(),
            actionable_query: if a.empty_evidence_window {
                question.to_string()
            } else {
                // t1d fix 3 (second-origin): the query form is chosen
                // with the corroboration record in view — a
                // floor-capped claim is queried as a FACT, not as the
                // prose cut (the query for the missing origin must
                // carry the figure the second origin must match).
                query_for(&a.claim, a.corroboration.as_ref())
            },
            from_claim_id: Some(format!("c{}", i + 1)),
            corroboration: a.corroboration.clone(),
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
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;

    #[test]
    fn sentence_splitter_attaches_spans() {
        let draft = "The Meridian Bridge was completed in 1873 [Source: https://example.com/a]. Its span is 240 meters [Source: https://example.com/b]. A final sentence with no citation.";
        let claims = split_claims(draft);
        assert_eq!(claims.len(), 3);
        assert!(claims[0].contains("1873"));
        assert!(claims[0].contains("[Source: https://example.com/a]"));
        assert!(!claims[1].contains("1873"));
        assert!(claims[1].contains("[Source: https://example.com/b]"));
        assert_eq!(claims[2], "A final sentence with no citation.");
    }

    #[test]
    fn empty_window_is_never_ran() {
        let audits = Vec::new();
        let gaps = build_gap_list("r", "h", 1, &audits, &[], "question?", &|_, _| {
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
        let g = build_gap_list("r", "h", 2, &[mk("a")], &prior, "question?", &|_, _| {
            "q".to_string()
        });
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
            &|_, _| "q".to_string(),
        );
        assert!(!g.strict_subset_of_prior);
        // A new gap (not in prior) → not a subset.
        let g = build_gap_list("r", "h", 2, &[mk("c")], &prior, "question?", &|_, _| {
            "q".to_string()
        });
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
            audit
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("ev-99")),
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
}
