// SPDX-License-Identifier: AGPL-3.0-or-later
//! R9 — the claim splitter's final pass: verdict set, the report, the
//! manifest.
//!
//! The final draft's claims are split with the same splitter the round
//! audits used (one splitter, two consumers) and re-audited against the
//! merged window. The report is verdict-stamped with chunk-level
//! citations; the four verdicts are distinct sections; could-not-judge
//! claims land in Open questions; never-ran claims land in Not
//! evaluated. Always flag, never remove: a failed claim is shown with
//! its flag, never deleted.

use super::audit::ClaimAudit;
use super::icd::{
    BudgetTotals, ClaimCitation, EvidenceWindow, FinalClaim, LockRecord, Manifest, ReframeRecord,
    RoundRow, SourceLedger, Verdict, VerdictSet,
};

/// Map the final audits to verdict-set rows, with C-class citations
/// resolved against the merged window.
pub fn final_claims(audits: &[ClaimAudit], window: &EvidenceWindow) -> Vec<FinalClaim> {
    audits
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let citations: Vec<ClaimCitation> = a
                .supporting_chunk_ids
                .iter()
                .filter_map(|cid| {
                    window
                        .chunks
                        .iter()
                        .find(|c| &c.id == cid)
                        .map(|c| ClaimCitation {
                            evidence_id: c.id.clone(),
                            url: c.source_url.clone(),
                            chunk_id: c.id.clone(),
                        })
                })
                .collect();
            let status = a.verdict.as_str().to_string();
            let flag = match a.verdict {
                Verdict::Failed => Some("refuted by the evidence".to_string()),
                Verdict::CouldNotJudge if a.witness.all_absent => {
                    Some("open question: extracted specifics absent from the evidence".to_string())
                }
                Verdict::CouldNotJudge => {
                    Some("open question: not judgeable from the evidence".to_string())
                }
                Verdict::NeverRan => {
                    Some("not evaluated: no evidence window was retrieved".to_string())
                }
                Verdict::Passed => None,
            };
            FinalClaim {
                id: format!("c{}", i + 1),
                text: a.claim.clone(),
                verdict: a.verdict,
                status,
                evidence_ids: a.supporting_chunk_ids.clone(),
                citations,
                flag,
            }
        })
        .collect()
}

/// The report — the product-shaped artifact. Verdict-stamped, four
/// verdicts as distinct sections, chunk-level citations, flags never
/// removed. A reframed run NAMES the substitution (§18.3): the report
/// answers the reframed question, and the original question stays
/// visible in the record — never a silent swap.
pub fn render_report(
    question: &str,
    claims: &[FinalClaim],
    run_id: &str,
    reframe: Option<&ReframeRecord>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {question}\n\n"));
    out.push_str(&format!(
        "- run: `{run_id}` — every claim below is verdict-stamped; citations are chunk-level.\n"
    ));
    if let Some(r) = reframe {
        out.push_str(&format!(
            "- re-framed at round {}: the loop was spinning (gap list unchanged, the last \
             acquire round fetched nothing); the question was reframed from `{}` to `{}`{}\n",
            r.round,
            r.original_question,
            r.reframed_question,
            if r.reason.is_empty() {
                String::new()
            } else {
                format!(" — {}", r.reason)
            }
        ));
    }
    out.push('\n');
    out.push_str("## Findings\n\n");
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut open = Vec::new();
    let mut not_evaluated = Vec::new();
    for c in claims {
        match c.verdict {
            Verdict::Passed => passed.push(c),
            Verdict::Failed => failed.push(c),
            Verdict::CouldNotJudge => open.push(c),
            Verdict::NeverRan => not_evaluated.push(c),
        }
    }
    for c in passed {
        out.push_str(&format!("- **[passed]** {}", c.text));
        if !c.citations.is_empty() {
            out.push_str(" — ");
            let sources: Vec<String> = c
                .citations
                .iter()
                .map(|cit| format!("`{}` [{}]({})", cit.evidence_id, cit.url, cit.url))
                .collect();
            out.push_str(&sources.join(", "));
        } else {
            out.push_str(" — *(judge-supported; no witnessable specifics — see verdict set)*");
        }
        out.push('\n');
    }
    out.push('\n');
    if !failed.is_empty() {
        out.push_str("## Refuted claims (flagged, never removed)\n\n");
        for c in failed {
            out.push_str(&format!(
                "- **[failed]** {} — *{}*\n",
                c.text,
                c.flag.as_deref().unwrap_or("refuted")
            ));
        }
        out.push('\n');
    }
    if !open.is_empty() {
        out.push_str("## Open questions\n\n");
        for c in open {
            out.push_str(&format!(
                "- **[could-not-judge]** {} — *{}*\n",
                c.text,
                c.flag.as_deref().unwrap_or("open")
            ));
        }
        out.push('\n');
    }
    if !not_evaluated.is_empty() {
        out.push_str("## Not evaluated\n\n");
        for c in not_evaluated {
            out.push_str(&format!(
                "- **[never-ran]** {} — *{}*\n",
                c.text,
                c.flag.as_deref().unwrap_or("not evaluated")
            ));
        }
        out.push('\n');
    }
    out
}

/// Everything the manifest needs at run close.
#[derive(Debug, Clone)]
pub struct ManifestInput {
    pub run_id: String,
    pub charter_hash: String,
    pub terminal_state: String,
    pub aborted_at_round: Option<u32>,
    pub truncation_declared: bool,
    pub rounds: Vec<RoundRow>,
    pub sources: SourceLedger,
    pub budget: BudgetTotals,
    pub not_covered: Vec<String>,
    /// GAP-4: the reframe record, when the run re-framed.
    pub reframe: Option<super::icd::ReframeRecord>,
    pub lock: LockRecord,
}

/// The run-close manifest ICD.
pub fn build_manifest(input: ManifestInput) -> Manifest {
    Manifest {
        icd: "manifest".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: input.run_id,
        charter_hash: input.charter_hash,
        terminal_state: input.terminal_state,
        aborted_at_round: input.aborted_at_round,
        truncation_declared: input.truncation_declared,
        rounds: input.rounds,
        sources: input.sources,
        budget: input.budget,
        not_covered: input.not_covered,
        reframe: input.reframe,
        lock: input.lock,
    }
}

/// Extract the not-covered list from the verdict set: the open
/// questions (could-not-judge) + the not-evaluated (never-ran). The
/// run-close record must not hide what stayed open.
pub fn not_covered(claims: &[FinalClaim]) -> Vec<String> {
    claims
        .iter()
        .filter(|c| matches!(c.verdict, Verdict::CouldNotJudge | Verdict::NeverRan))
        .map(|c| c.text.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Custody;

    fn window() -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: vec![super::super::icd::WindowChunk {
                id: "ev-1".to_string(),
                locator: "https://example.com/a".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: Custody::PublicWeb.as_str().to_string(),
                provenance_class: "known".to_string(),
                content: "The Meridian Bridge was completed in 1873.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
            fetch_failures: Vec::new(),
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        }
    }

    #[test]
    fn report_is_verdict_stamped_with_citations() {
        let audits = vec![
            ClaimAudit {
                claim: "The bridge was completed in 1873.".to_string(),
                verdict: Verdict::Passed,
                action: super::super::icd::GateAction::CitationGrounded,
                witness: Default::default(),
                supporting_chunk_ids: vec!["ev-1".to_string()],
                empty_evidence_window: false,
                reason: None,
            },
            ClaimAudit {
                claim: "The engineer was Helena Voss.".to_string(),
                verdict: Verdict::CouldNotJudge,
                action: super::super::icd::GateAction::AbstainedDecline,
                witness: Default::default(),
                supporting_chunk_ids: Vec::new(),
                empty_evidence_window: false,
                reason: None,
            },
        ];
        let claims = final_claims(&audits, &window());
        let report = render_report("Meridian Bridge history", &claims, "run-1", None);
        assert!(report.contains("[passed]"));
        assert!(report.contains("https://example.com/a"));
        assert!(report.contains("Open questions"));
        assert!(report.contains("[could-not-judge]"));
        assert!(!report.contains("[never-ran]"));
        assert_eq!(claims[0].citations.len(), 1);
        assert_eq!(claims[0].citations[0].evidence_id, "ev-1");
    }

    #[test]
    fn a_reframed_run_names_the_substitution() {
        // GAP-4/§18.3: the report answers the reframed question and
        // must NAME the swap — the original question stays visible in
        // the record, never silently replaced.
        let claims = final_claims(&[], &window());
        let report = render_report(
            "Why is the bridge kept lit at night?",
            &claims,
            "run-1",
            Some(&ReframeRecord {
                icd: "reframe".to_string(),
                version: 1,
                run_id: "run-1".to_string(),
                charter_hash: "h".to_string(),
                round: 2,
                original_question: "When was the bridge built?".to_string(),
                reframed_question: "Why is the bridge kept lit at night?".to_string(),
                reason: "the loop spun".to_string(),
                trigger: "structural surprise".to_string(),
            }),
        );
        assert!(report.starts_with("# Why is the bridge kept lit at night?"));
        assert!(report.contains("re-framed at round 2"));
        assert!(report.contains("`When was the bridge built?`"));
        assert!(report.contains("the loop spun"));
    }

    #[test]
    fn never_ran_lands_in_not_covered() {
        let audits = vec![ClaimAudit {
            claim: "Unanswerable without evidence.".to_string(),
            verdict: Verdict::NeverRan,
            action: super::super::icd::GateAction::AbstainedDecline,
            witness: Default::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: true,
            reason: None,
        }];
        let claims = final_claims(&audits, &window());
        assert_eq!(
            not_covered(&claims),
            vec!["Unanswerable without evidence.".to_string()]
        );
        let report = render_report("Q", &claims, "run-1", None);
        assert!(report.contains("Not evaluated"));
        assert!(report.contains("[never-ran]"));
        // Could-not-judge claims are covered by not_covered too — the
        // run-close record must not hide the open questions.
        let audits = vec![ClaimAudit {
            claim: "Funding is unclear.".to_string(),
            verdict: Verdict::CouldNotJudge,
            action: super::super::icd::GateAction::AbstainedDecline,
            witness: Default::default(),
            supporting_chunk_ids: Vec::new(),
            empty_evidence_window: false,
            reason: None,
        }];
        let claims = final_claims(&audits, &window());
        assert_eq!(
            not_covered(&claims),
            vec!["Funding is unclear.".to_string()]
        );
    }
}
