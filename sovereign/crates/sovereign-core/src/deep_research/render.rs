// SPDX-License-Identifier: AGPL-3.0-or-later
//! R9 — the claim splitter's final pass: verdict set, the report, the
//! manifest.
//!
//! The final draft's claims are split with the same splitter the round
//! audits used (one splitter, two consumers) and re-audited against the
//! merged window. The report is verdict-stamped with chunk-level
//! citations; the four verdicts are distinct sections; could-not-judge
//! claims land in Open questions — EXCEPT the drb1-r3b graded tier: a
//! could-not-judge claim whose RECORDED flag names single-origin
//! support (corroboration floor) renders under Findings stamped
//! `[single-origin]`, never `[passed]` (the verdict stands; the page
//! presents the claim, floor-capped, instead of walling it). Never-ran
//! claims land in Not evaluated. Always flag, never remove: a failed
//! claim is shown with its flag, never deleted.

use super::audit::ClaimAudit;
use super::containment::strip_citation_spans;
use super::icd::{
    AlignmentRecord, BudgetTotals, ClaimCitation, EmptyRound, EmptyRoundReason, EvidenceWindow,
    FinalClaim, GateAction, LockRecord, Manifest, ReframeRecord, ResidueRow, RoundRow,
    SourceLedger, Verdict, VerdictSet,
};

// ---------------------------------------------------------------------
// drb1-r3b — the recorded-flag vocabulary. `final_claims` (below) is
// the single PRODUCER of these strings; `grade_recorded_flag` (further
// below) is the single CONSUMER that grades a render tier by them.
// Consts, not inline literals, so producer and grader share one name
// per string (§10.6: one implementation per key). Measured vocabulary
// 2026-08-21 across every verdict-set.json on disk (168 files): five
// distinct flags — 1108 single-origin, 727 specifics-absent, 124
// refuted, 107 not-judgeable, 69 no-citation-handle — a closed set,
// each an enumerated arm of the producer's match.
// ---------------------------------------------------------------------

const FLAG_REFUTED: &str = "refuted by the evidence";
const FLAG_NO_CITATION_HANDLE: &str =
    "open question: no citation handle (ref-required — the draft must cite the chunks it asserts against)";
const FLAG_UNRESOLVABLE_HANDLE: &str =
    "open question: the citation handle does not name a window chunk (ref-required)";
const FLAG_SINGLE_ORIGIN: &str = "open question: single-origin support (corroboration floor)";
/// drb1-t5 (§18.3): a floor-capped claim whose support set located ZERO
/// origins is not "single-origin" — measured 2026-08-22 across the t7a
/// flight, 63 of the 72 claims stamped FLAG_SINGLE_ORIGIN carried an
/// empty `corroboration.origins`. The flag reported an absence as a
/// count. Absence gets its own name and stays walled.
const FLAG_NO_ORIGIN: &str = "open question: no supporting origin located (corroboration floor)";
const FLAG_SPECIFICS_ABSENT: &str = "open question: extracted specifics absent from the evidence";
const FLAG_NOT_JUDGEABLE: &str = "open question: not judgeable from the evidence";
const FLAG_NOT_EVALUATED: &str = "not evaluated: no evidence window was retrieved";

/// Map the final audits to verdict-set rows, with C-class citations
/// resolved against the merged window.
pub fn final_claims(audits: &[ClaimAudit], window: &EvidenceWindow) -> Vec<FinalClaim> {
    audits
        .iter()
        .enumerate()
        .map(|(i, a)| {
            // R3a citation registry: citations render only from captured
            // evidence ids. Orphan citations (chunk ids not in the window)
            // are glassbox WARN + omitted, never silently kept.
            let mut orphan_count = 0;
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
            orphan_count = a.supporting_chunk_ids.len() - citations.len();
            if orphan_count > 0 {
                tracing::warn!(
                    target: "deep_research",
                    claim_index = i,
                    orphan_count,
                    total_referenced = a.supporting_chunk_ids.len(),
                    "citation registry: {orphan_count} orphan citation(s) omitted (chunk id not in evidence window)"
                );
            }
            let status = a.verdict.as_str().to_string();
            let flag = match a.verdict {
                Verdict::Failed => Some(FLAG_REFUTED.to_string()),
                // REF-REQUIRED (order deep-research-t4a): the refusal
                // classes name the cause — the reader sees the draft's
                // selection failed the gate, not a generic
                // could-not-judge.
                Verdict::CouldNotJudge if a.action == GateAction::RefusedNoCitationHandle => {
                    Some(FLAG_NO_CITATION_HANDLE.to_string())
                }
                Verdict::CouldNotJudge if a.action == GateAction::RefusedUnresolvableHandle => {
                    Some(FLAG_UNRESOLVABLE_HANDLE.to_string())
                }
                // GAP-2: a floor-capped claim names the cause — the
                // reader sees WHY the claim is open (single origin),
                // not a generic could-not-judge.
                Verdict::CouldNotJudge
                    if a.corroboration.as_ref().is_some_and(|r| !r.passes_floor) =>
                {
                    // §18.3: report the absence, never a count the record
                    // does not carry. ONE origin is single-origin; ZERO
                    // origins is an unlocated support set, and saying
                    // "single-origin" of it is a false record.
                    let located = a.corroboration.as_ref().map_or(0, |r| r.origins.len());
                    if located >= 1 {
                        Some(FLAG_SINGLE_ORIGIN.to_string())
                    } else {
                        Some(FLAG_NO_ORIGIN.to_string())
                    }
                }
                Verdict::CouldNotJudge if a.witness.all_absent => {
                    Some(FLAG_SPECIFICS_ABSENT.to_string())
                }
                Verdict::CouldNotJudge => Some(FLAG_NOT_JUDGEABLE.to_string()),
                Verdict::NeverRan => Some(FLAG_NOT_EVALUATED.to_string()),
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
                corroboration: a.corroboration.clone(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------
// drb1-r3b — flag→tier grading (the measured lever).
// ---------------------------------------------------------------------

/// The render grade of a could-not-judge claim's RECORDED flag.
///
/// The R3a scan (2026-08-21) measured the wall: the `[single-origin]`
/// tier R3a built was EMPTY on flight data because the walled mass is
/// could-not-judge verdicts stamped at audit time — and the flight's
/// recorded flags decompose 72/137 single-origin-capped (substance
/// present, one origin, floor unmet) vs 54 specifics-absent (the
/// honest wall) vs 11 other. The flag is the recorded REASON after the
/// producer's precedence — so the grader follows the record, never
/// re-derives from the structured fields (a no-handle claim with a
/// failing floor record records the no-handle flag, and stays walled).
enum FlagGrade {
    /// Substance present, single origin, corroboration floor unmet —
    /// renders in Findings under the `[single-origin]` tier, stamped
    /// `[single-origin]` and NEVER `[passed]`.
    SingleOrigin,
    /// The wall holds — renders in Open questions.
    Walled,
}

/// THE match site that maps a recorded could-not-judge flag to its
/// render grade (§2.1: a match on string ids, kept consciously small —
/// one graded arm, four walled arms from the measured closed set,
/// unknown + absent arms both walled). Unknown flags default WALLED
/// with a glassbox WARN — never silently graded up (§18.3).
fn grade_recorded_flag(flag: Option<&str>, claim_id: &str) -> FlagGrade {
    match flag {
        Some(FLAG_SINGLE_ORIGIN) => FlagGrade::SingleOrigin,
        Some(
            FLAG_SPECIFICS_ABSENT
            | FLAG_NO_CITATION_HANDLE
            | FLAG_UNRESOLVABLE_HANDLE
            | FLAG_NOT_JUDGEABLE
            | FLAG_NO_ORIGIN,
        ) => FlagGrade::Walled,
        Some(unknown) => {
            tracing::warn!(
                target: "deep_research",
                claim = claim_id,
                flag = unknown,
                "flag-graded render: unknown flag defaults WALLED (never silently graded up)"
            );
            FlagGrade::Walled
        }
        None => FlagGrade::Walled,
    }
}

/// The render tier — ONE decider, one name (§10.6): `render_report`
/// and `render_race` classify through this enum, never their own
/// copies of the split. Verdicts, the floor, and audit semantics are
/// untouched — this is presentation tiering only.
enum RenderTier {
    /// Passed with the floor met — an anchor, no tier label.
    Corroborated,
    /// Passed on a single origin — `[passed] [single-origin]`.
    PassedSingleOrigin,
    /// Could-not-judge at the corroboration floor, graded by its
    /// recorded flag — `[single-origin]`, never `[passed]`.
    FlagGradedSingleOrigin,
    Refuted,
    OpenQuestion,
    NotEvaluated,
}

fn render_tier(c: &FinalClaim) -> RenderTier {
    match c.verdict {
        Verdict::Passed if c.corroboration.as_ref().is_some_and(|r| r.passes_floor) => {
            RenderTier::Corroborated
        }
        Verdict::Passed => RenderTier::PassedSingleOrigin,
        Verdict::CouldNotJudge => match grade_recorded_flag(c.flag.as_deref(), &c.id) {
            FlagGrade::SingleOrigin => RenderTier::FlagGradedSingleOrigin,
            FlagGrade::Walled => RenderTier::OpenQuestion,
        },
        Verdict::Failed => RenderTier::Refuted,
        Verdict::NeverRan => RenderTier::NotEvaluated,
    }
}

/// The tail a Findings row carries after its text: typed citations
/// first; absent those, the floor record's named origins (the single
/// origin, named — better than an absence note when the record has
/// it); absent both, the honest-absence note named by the caller
/// (passed rows and graded rows word it differently).
fn findings_tail(c: &FinalClaim, no_citation_note: &str) -> String {
    if !c.citations.is_empty() {
        let sources: Vec<String> = c
            .citations
            .iter()
            .map(|cit| format!("`{}` [{}]({})", cit.evidence_id, cit.url, cit.url))
            .collect();
        format!(" — {}", sources.join(", "))
    } else if let Some(origins) = c
        .corroboration
        .as_ref()
        .filter(|r| !r.origins.is_empty())
        .map(|r| &r.origins)
    {
        let named: Vec<String> = origins.iter().map(|u| format!("[{u}]({u})")).collect();
        format!(" — origin: {}", named.join(", "))
    } else {
        format!(" — *{no_citation_note}*")
    }
}

const NOTE_JUDGE_SUPPORTED: &str = "judge-supported; no witnessable specifics — see verdict set";
const NOTE_SINGLE_ORIGIN_UNCITED: &str = "single origin; no witnessable citation — see verdict set";
// drb1-r3b follow-up (seat finding, 2026-08-21): presentation prose
// never spells a stamp in brackets — the campaign bar's regex counts
// raw bracket-stamps, so a legend like "[single-origin]" adds
// non-verdict markers to the denominator (measured: 8 legends × 2
// stamps = +16, regex read 55/153 vs the true per-claim 55/137).
// Stamps live on claim rows only; prose names them quoted.
const FINDINGS_GRADED_NOTE: &str = "*Rows stamped 'single-origin' without 'passed' are \
     could-not-judge at the corroboration floor: one origin supports the claim's substance, \
     the floor requires two. The verdict stands in the verdict set; the page presents the \
     claim, floor-capped, instead of walling it.*";

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
    alignment: Option<&AlignmentRecord>,
    residue: &[ResidueRow],
    empty_rounds: &[EmptyRound],
) -> String {
    // Model-written `[Source: …]` tails are demoted at the page (order
    // deep-research-t4a, pre-registered — pass site 1: 83% of claims
    // rendered verbatim with their tails): the typed citation channel
    // is the only citation that ships. The verdict-set side is
    // untouched — final_claims keeps the raw text, and the DRB
    // scorer's Amendment-3 pair formation resolves tails through the
    // draft registry (named, deliberate).
    let claims: Vec<FinalClaim> = claims
        .iter()
        .map(|c| FinalClaim {
            text: strip_citation_spans(&c.text),
            ..c.clone()
        })
        .collect();
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
    // STEER 2: a redirected run NAMES the substitution (§18.3) — the
    // report answers the redirected question, and the original question
    // stays visible in the record (alignment-1.json + this line). Never
    // a silent swap at the gate.
    if let Some(a) = alignment {
        out.push_str(&format!(
            "- redirected at alignment (round 0, pre-acquisition): the question was \
             redirected from `{}` to `{}`{}\n",
            a.original_question,
            a.redirected_question,
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(" — {}", a.reason)
            }
        ));
    }
    out.push('\n');
    out.push_str("## Findings\n\n");
    // drb1-r3b flag-graded render: one decider (`render_tier`) feeds
    // both renderers. Passed splits corroborated / single-origin (the
    // R3a tiers); could-not-judge claims grade by their RECORDED flag —
    // single-origin-capped substance renders here, floor-capped, every
    // other recorded flag stays walled.
    let mut corroborated = Vec::new();
    let mut passed_single_origin = Vec::new();
    let mut graded_single_origin = Vec::new();
    let mut failed = Vec::new();
    let mut open = Vec::new();
    let mut not_evaluated = Vec::new();
    for c in claims {
        match render_tier(&c) {
            RenderTier::Corroborated => corroborated.push(c),
            RenderTier::PassedSingleOrigin => passed_single_origin.push(c),
            RenderTier::FlagGradedSingleOrigin => graded_single_origin.push(c),
            RenderTier::Refuted => failed.push(c),
            RenderTier::OpenQuestion => open.push(c),
            RenderTier::NotEvaluated => not_evaluated.push(c),
        }
    }
    // The graded-tier note renders ONLY when graded rows exist — a
    // verdict set with none renders byte-identically to the pre-r3b
    // page (the reframe/align goldens and no-cap flights stay pinned).
    if !graded_single_origin.is_empty() {
        out.push_str(FINDINGS_GRADED_NOTE);
        out.push_str("\n\n");
    }
    // Corroborated findings first (no tier label — anchors,
    // two-origin passed).
    for c in corroborated {
        out.push_str(&format!(
            "- **[passed]** {}{}\n",
            c.text,
            findings_tail(&c, NOTE_JUDGE_SUPPORTED)
        ));
    }
    // Passed on a single origin — tier-labeled.
    for c in passed_single_origin {
        out.push_str(&format!(
            "- **[passed] [single-origin]** {}{}\n",
            c.text,
            findings_tail(&c, NOTE_JUDGE_SUPPORTED)
        ));
    }
    // drb1-r3b: could-not-judge at the floor, graded by its recorded
    // flag — `[single-origin]`, never `[passed]` (the verdict stands;
    // the page names the tier and the floor, §18.3).
    for c in graded_single_origin {
        let note = format!(
            "{}; verdict stands could-not-judge",
            flag_without_class_prefix(
                c.flag.as_deref().unwrap_or(FLAG_SINGLE_ORIGIN),
                "open question"
            )
        );
        let row = format!(
            "- **[single-origin]** {} — *{note}*{}\n",
            c.text,
            findings_tail(&c, NOTE_SINGLE_ORIGIN_UNCITED)
        );
        out.push_str(&row);
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
    // GAP-3: the epistemic residue — every searched-but-absent query is
    // first-class report content ("we looked for X and found no
    // evidence either way"). Publication-bias awareness: the absence is
    // disclosed, never absorbed into a silent gap between the search
    // and the finding. Empty residue renders NO section (a run where
    // every search found something has nothing to disclose).
    if !residue.is_empty() {
        out.push_str("## Searched but absent\n\n");
        out.push_str(
            "The queries below were executed and returned no evidence. An absence is a \
             finding, not a failure — we looked for these and found no evidence either way.\n\n",
        );
        for row in residue {
            out.push_str(&format!(
                "- round {}: \"{}\" — searched, no evidence returned\n",
                row.round, row.query
            ));
        }
        out.push('\n');
    }
    // T7b (order deep-research-t7b, pre-registered): the round-level
    // "no evidence fetched" state — the verdict assembly had no reader
    // for it (rounds whose fetches were all dedup-refused rendered
    // identically to rounds that never added evidence). The section
    // follows the residue pattern: every empty round is first-class
    // report content naming its reason (the closed enum), and an empty
    // rounds list renders NO section — a run where every round added
    // evidence has nothing to disclose (keeps the goldens byte-pinned).
    if !empty_rounds.is_empty() {
        out.push_str("## No evidence fetched\n\n");
        out.push_str(
            "The rounds below added no evidence: the round's fetch yield was empty, so no \
             claim could be judged on new material from that round.\n\n",
        );
        for er in empty_rounds {
            out.push_str(&format!(
                "- round {}: no evidence was added this round — {}\n",
                er.round,
                er.reason.as_str()
            ));
        }
        out.push('\n');
    }
    out
}

/// The flag's leading class word repeats the stamp ("open question: …",
/// "not evaluated: …") — the page names the class once, in the stamp;
/// the explanation that follows is preserved verbatim.
fn flag_without_class_prefix<'a>(flag: &'a str, class: &str) -> &'a str {
    let prefix = format!("{class}: ");
    match flag.strip_prefix(&prefix) {
        Some(rest) => rest,
        None => flag,
    }
}

/// The clean article page — the RACE scorer's input surface (order
/// deep-research-t6b, pre-window slice, pre-registered 2026-08-19).
/// Passed findings organized by section, every claim carrying its
/// TYPED citations from the structured channel (evidence id + source
/// URL); zero bare model-written tails in [passed] position;
/// downgraded claims visibly stamped, never removed. report.md (the
/// verdict transcript) is UNCHANGED — this is a post-flight rendering
/// pass over the same verdict set, not a judgment change.
pub fn render_race(question: &str, claims: &[FinalClaim], run_id: &str) -> String {
    // Model-written `[Source: …]` tails are demoted at the page (the
    // same pass the transcript runs): the typed citation channel is
    // the only citation that ships. The verdict-set side is untouched.
    let claims: Vec<FinalClaim> = claims
        .iter()
        .map(|c| FinalClaim {
            text: strip_citation_spans(&c.text),
            ..c.clone()
        })
        .collect();
    let established = claims
        .iter()
        .filter(|c| c.verdict == Verdict::Passed)
        .count();
    // drb1-r3b: the flag-graded mass is counted and named separately —
    // a floor-capped row is not a passed finding and must not inflate
    // the established count, nor hide inside the open count.
    let graded = claims
        .iter()
        .filter(|c| matches!(render_tier(c), RenderTier::FlagGradedSingleOrigin))
        .count();
    let open = claims.len() - established - graded;
    let mut out = String::new();
    out.push_str(&format!("# {question}\n\n"));
    out.push_str(&format!(
        "_run: `{run_id}` — {established} finding{} established; {}{open} claim{} open. \
         Citations are chunk-level (evidence id + source URL)._\n\n",
        if established == 1 { "" } else { "s" },
        if graded > 0 {
            format!(
                "{graded} single-origin floor-capped (could-not-judge at the corroboration \
                 floor); "
            )
        } else {
            String::new()
        },
        if open == 1 { "" } else { "s" },
    ));
    // drb1-r3b flag-graded render: the same decider as render_report.
    let mut corroborated = Vec::new();
    let mut passed_single_origin = Vec::new();
    let mut graded_single_origin = Vec::new();
    let mut failed = Vec::new();
    let mut open_q = Vec::new();
    let mut not_evaluated = Vec::new();
    for c in claims {
        match render_tier(&c) {
            RenderTier::Corroborated => corroborated.push(c),
            RenderTier::PassedSingleOrigin => passed_single_origin.push(c),
            RenderTier::FlagGradedSingleOrigin => graded_single_origin.push(c),
            RenderTier::Refuted => failed.push(c),
            RenderTier::OpenQuestion => open_q.push(c),
            RenderTier::NotEvaluated => not_evaluated.push(c),
        }
    }
    out.push_str("## Findings\n\n");
    if !graded_single_origin.is_empty() {
        out.push_str(FINDINGS_GRADED_NOTE);
        out.push_str("\n\n");
    }
    // Corroborated findings first (no tier label).
    for c in corroborated {
        out.push_str(&format!(
            "- **[passed]** {}{}\n",
            c.text,
            findings_tail(&c, NOTE_JUDGE_SUPPORTED)
        ));
    }
    // Passed on a single origin — tier-labeled.
    for c in passed_single_origin {
        out.push_str(&format!(
            "- **[passed] [single-origin]** {}{}\n",
            c.text,
            findings_tail(&c, NOTE_JUDGE_SUPPORTED)
        ));
    }
    // drb1-r3b: the graded tier, same row shape as the report page.
    for c in graded_single_origin {
        let note = format!(
            "{}; verdict stands could-not-judge",
            flag_without_class_prefix(
                c.flag.as_deref().unwrap_or(FLAG_SINGLE_ORIGIN),
                "open question"
            )
        );
        let row = format!(
            "- **[single-origin]** {} — *{note}*{}\n",
            c.text,
            findings_tail(&c, NOTE_SINGLE_ORIGIN_UNCITED)
        );
        out.push_str(&row);
    }
    out.push('\n');
    if !failed.is_empty() {
        out.push_str("## Refuted claims\n\n");
        for c in failed {
            out.push_str(&format!(
                "- **[refuted]** {} — *{}*\n",
                c.text,
                flag_without_class_prefix(c.flag.as_deref().unwrap_or("refuted"), "refuted")
            ));
        }
        out.push('\n');
    }
    if !open_q.is_empty() {
        out.push_str("## Open questions\n\n");
        for c in open_q {
            out.push_str(&format!(
                "- **[open question]** {} — *{}*\n",
                c.text,
                flag_without_class_prefix(c.flag.as_deref().unwrap_or("open"), "open question")
            ));
        }
        out.push('\n');
    }
    if !not_evaluated.is_empty() {
        out.push_str("## Not evaluated\n\n");
        for c in not_evaluated {
            out.push_str(&format!(
                "- **[not evaluated]** {} — *{}*\n",
                c.text,
                flag_without_class_prefix(
                    c.flag.as_deref().unwrap_or("not evaluated"),
                    "not evaluated"
                )
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
    /// STEER 2: the alignment record, when the pre-acquisition gate
    /// redirected the question.
    pub alignment: Option<super::icd::AlignmentRecord>,
    /// GAP-3: the epistemic residue — every searched-but-absent query.
    pub residue: Vec<ResidueRow>,
    /// The run-scoped consent grant (order deep-research-t2a) — the
    /// manifest record of what the operator released for this run.
    pub consent: Option<crate::egress::ConsentGrant>,
    pub lock: LockRecord,
}

/// The run-close manifest ICD.

/// drb1-t5: annotate a COMPOSED report with its verdicts.
///
/// The claim-ledger render rebuilt the page out of audited claim rows,
/// so a claim that could not be verified took the answer down with it —
/// measured on the logged t7a flight, 127 of 137 claims landed
/// could-not-judge and every `## Findings` section rendered empty.
///
/// Here the article IS the deliverable and the verdicts ANNOTATE it. The
/// honesty property is unchanged and is still enforced, not instructed:
/// a refuted claim is marked in place so it cannot stand as an
/// assertion, and everything the gate could not verify is named in a
/// closing section. Nothing is silently dropped (§18.3).
pub fn annotate_composed(report: &str, claims: &[FinalClaim]) -> String {
    let mut out = report.to_string();

    // Refuted claims are marked WHERE THEY STAND — an unmarked refuted
    // sentence would be the page asserting something the evidence
    // contradicts.
    //
    // THE NEEDLE AND THE HAYSTACK ARE NUMBERED DIFFERENTLY, and matching them
    // raw silently marked NOTHING. A claim is extracted from the PRE-render
    // draft, so it carries the internal handle: "…landscape. [Source: ev-13]".
    // The report it is matched against has already been through
    // `number_citations`, so the same sentence reads "…landscape. [13]". An
    // exact `find` therefore fails on every claim that ends in a citation —
    // which is nearly all of them, because the writer is instructed to cite as
    // it goes. Measured on run dr-1787768404: 192 refuted claims, 192 of them
    // silently unmarked, and the only disclosure a count at line 293 of 339,
    // BELOW the Sources list. The report asserted all 192 as fact.
    //
    // So the match runs on the citation-stripped stem, via the existing
    // decider (`containment::strip_citation_spans`, §10.6/§19 — the same one
    // the gap-query path reuses) rather than a second bracket parser here.
    // The MARKER still lands at the end of the sentence as it appears in the
    // report, so the reader sees it where the claim stands.
    let mut marked = 0usize;
    let mut unmarkable = 0usize;
    for c in claims.iter().filter(|c| c.verdict == Verdict::Failed) {
        let stem = super::containment::strip_citation_spans(&c.text);
        let needle = stem.trim();
        if needle.len() < 12 {
            unmarkable += 1;
            continue;
        }
        if let Some(at) = out.find(needle) {
            let end = at + needle.len();
            out.insert_str(end, " **[refuted by the evidence]**");
            marked += 1;
        } else {
            unmarkable += 1;
        }
    }
    // A REFUTATION THAT DOES NOT REACH THE PAGE IS NOT A REFUTATION (§18.1's
    // four verdicts, §18.3's no-silent-substitution). The failure above was
    // invisible for exactly as long as nobody counted, so the count is now
    // part of the render and the reader is told rather than the log.
    if unmarkable > 0 {
        tracing::warn!(
            target: "deep_research",
            refuted = marked + unmarkable,
            marked,
            unmarkable,
            "refuted claims that could not be located in the rendered report — \
             they are reported in the Verification section, never dropped"
        );
    }

    let passed = claims
        .iter()
        .filter(|c| c.verdict == Verdict::Passed)
        .count();
    let refuted = claims
        .iter()
        .filter(|c| c.verdict == Verdict::Failed)
        .count();
    let open: Vec<&FinalClaim> = claims
        .iter()
        .filter(|c| c.verdict == Verdict::CouldNotJudge)
        .collect();

    out.push_str("\n\n## Verification\n\n");
    // SAY WHAT HAPPENED, NOT WHAT WAS INTENDED. This sentence used to read
    // "{refuted} were refuted by the evidence and are marked in place"
    // unconditionally — and on every run where the marking silently missed,
    // that sentence was itself the report's least true claim. It now counts
    // the markings it actually made.
    out.push_str(&format!(
        "Of {} claims extracted from this report, {passed} verified against two or more \
         independent sources, {refuted} were refuted by the evidence, and {} could not be \
         verified from the evidence gathered.\n",
        claims.len(),
        open.len()
    ));
    if refuted > 0 {
        out.push_str(&format!(
            "\nOf the {refuted} refuted, {marked} are marked in place where they stand.\n"
        ));
        if unmarkable > 0 {
            out.push_str(&format!(
                "The remaining {unmarkable} could not be located in the finished text to be \
                 marked, so they are listed here instead. **Read them as contradicted by the \
                 evidence wherever they appear above:**\n\n"
            ));
            for c in claims
                .iter()
                .filter(|c| c.verdict == Verdict::Failed)
                .take(40)
            {
                let stem = super::containment::strip_citation_spans(&c.text);
                let short: String = stem.trim().chars().take(220).collect();
                if !short.is_empty() {
                    out.push_str(&format!("- {short}\n"));
                }
            }
            if unmarkable > 40 {
                out.push_str(&format!(
                    "- …and {} further refuted statements, all recorded in the verdict set.\n",
                    unmarkable - 40
                ));
            }
        }
    }
    if !open.is_empty() {
        out.push_str(
            "\nThe following statements rest on evidence the gate could not confirm. They are \
             reported rather than removed, and should be read as unverified:\n\n",
        );
        for c in open.iter().take(40) {
            let t = c.text.trim();
            let short: String = t.chars().take(220).collect();
            out.push_str(&format!("- {short}\n"));
        }
        if open.len() > 40 {
            out.push_str(&format!(
                "- …and {} further unverified statements, all recorded in the verdict set.\n",
                open.len() - 40
            ));
        }
    }
    out
}

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
        alignment: input.alignment,
        residue: input.residue,
        consent: input.consent,
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
            dedup_refused: Vec::new(),
            content_refused: Vec::new(),
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
                corroboration: None,
            },
            ClaimAudit {
                claim: "The engineer was Helena Voss.".to_string(),
                verdict: Verdict::CouldNotJudge,
                action: super::super::icd::GateAction::AbstainedDecline,
                witness: Default::default(),
                supporting_chunk_ids: Vec::new(),
                empty_evidence_window: false,
                reason: None,
                corroboration: None,
            },
        ];
        let claims = final_claims(&audits, &window());
        let report = render_report(
            "Meridian Bridge history",
            &claims,
            "run-1",
            None,
            None,
            &[],
            &[],
        );
        assert!(report.contains("[passed]"));
        assert!(report.contains("https://example.com/a"));
        assert!(report.contains("Open questions"));
        assert!(report.contains("[could-not-judge]"));
        assert!(!report.contains("[never-ran]"));
        assert_eq!(claims[0].citations.len(), 1);
        assert_eq!(claims[0].citations[0].evidence_id, "ev-1");
    }

    /// RED (order deep-research-t4a, pre-registered — pass site 1: 83%
    /// of claims rendered verbatim with model-written tails): the
    /// report never ships a model-written `[Source: …]` tail; the
    /// typed citation channel is the only citation on the page. The
    /// verdict-set side keeps the raw text — the DRB scorer's
    /// Amendment-3 pair formation resolves tails through the draft
    /// registry (named, deliberate).
    #[test]
    fn report_strips_model_tails_keeps_typed_citations() {
        let audits = vec![ClaimAudit {
            claim: "The bridge was completed in 1873 [Source: https://example.com/draft]. "
                .to_string(),
            verdict: Verdict::Passed,
            action: super::super::icd::GateAction::CitationGrounded,
            witness: Default::default(),
            supporting_chunk_ids: vec!["ev-1".to_string()],
            empty_evidence_window: false,
            reason: None,
            corroboration: None,
        }];
        let claims = final_claims(&audits, &window());
        let report = render_report(
            "Meridian Bridge history",
            &claims,
            "run-1",
            None,
            None,
            &[],
            &[],
        );
        assert!(
            !report.contains("[Source:"),
            "model-written tails never ship on the page"
        );
        assert!(
            report.contains("https://example.com/a"),
            "the typed citation still renders"
        );
        assert!(report.contains("[passed]"));
        // The verdict-set side keeps the raw text — the scorer's pair
        // formation depends on it.
        assert_eq!(
            claims[0].text,
            "The bridge was completed in 1873 [Source: https://example.com/draft]. "
        );
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
            None,
            &[],
            &[],
        );
        assert!(report.starts_with("# Why is the bridge kept lit at night?"));
        assert!(report.contains("re-framed at round 2"));
        assert!(report.contains("`When was the bridge built?`"));
        assert!(report.contains("the loop spun"));
    }

    #[test]
    fn a_redirected_run_names_the_substitution() {
        // STEER 2/§18.3: the report answers the redirected question and
        // must NAME the swap at the gate — the original question stays
        // visible in the record, never silently replaced.
        let claims = final_claims(&[], &window());
        let report = render_report(
            "What did OpenAI and Anthropic do in March 2025?",
            &claims,
            "run-1",
            None,
            Some(&AlignmentRecord {
                icd: "alignment".to_string(),
                version: 1,
                run_id: "run-1".to_string(),
                charter_hash: "h".to_string(),
                round: 0,
                original_question: "When was the bridge built?".to_string(),
                redirected_question: "What did OpenAI and Anthropic do in March 2025?".to_string(),
                reason: "the plan spends on the wrong acquisition target".to_string(),
                trigger: "pre-acquisition alignment".to_string(),
            }),
            &[],
            &[],
        );
        assert!(report.starts_with("# What did OpenAI and Anthropic do in March 2025?"));
        assert!(report.contains("redirected at alignment (round 0, pre-acquisition)"));
        assert!(report.contains("`When was the bridge built?`"));
        assert!(report.contains("the plan spends on the wrong acquisition target"));
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
            corroboration: None,
        }];
        let claims = final_claims(&audits, &window());
        assert_eq!(
            not_covered(&claims),
            vec!["Unanswerable without evidence.".to_string()]
        );
        let report = render_report("Q", &claims, "run-1", None, None, &[], &[]);
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
            corroboration: None,
        }];
        let claims = final_claims(&audits, &window());
        assert_eq!(
            not_covered(&claims),
            vec!["Funding is unclear.".to_string()]
        );
    }

    // ------------------------------------------------------------------
    // GAP-3 — the epistemic-residue section (RED-FIRST: the section is
    // absent at HEAD; these tests watched the red before the renderer
    // change landed).
    // ------------------------------------------------------------------

    /// Every searched-but-absent query is first-class report content:
    /// the section exists, names every query, and says what the absence
    /// means — never a silent gap between "we searched" and "we found".
    #[test]
    fn residue_section_renders_every_searched_but_absent_query() {
        let claims = final_claims(&[], &window());
        let residue = [
            ResidueRow {
                query: "What did OpenAI and Anthropic do in March 2025?".to_string(),
                round: 1,
            },
            ResidueRow {
                query: "Anthropic safety team acquisition 2025".to_string(),
                round: 2,
            },
        ];
        let report = render_report("Q", &claims, "run-1", None, None, &residue, &[]);
        assert!(
            report.contains("## Searched but absent"),
            "the searched-but-absent section must render: {report}"
        );
        for row in &residue {
            assert!(
                report.contains(&row.query),
                "every empty-result query must be named in the section: {report}"
            );
        }
        assert!(
            report.contains("found no evidence either way"),
            "the section must say what the absence means"
        );
        // The round is on the row — the reader sees when the search ran.
        assert!(report.contains("round 1"));
    }

    /// Empty residue renders NO section — a run where every search
    /// found something has nothing to disclose, and the report must not
    /// grow a vestigial heading (keeps the meridian/reframe/align
    /// goldens byte-pinned).
    #[test]
    fn empty_residue_renders_no_section() {
        let claims = final_claims(&[], &window());
        let report = render_report("Q", &claims, "run-1", None, None, &[], &[]);
        assert!(
            !report.contains("Searched but absent"),
            "an empty residue must render no section: {report}"
        );
    }

    // ------------------------------------------------------------------
    // T7b — the "No evidence fetched" section (RED-FIRST: the section
    // and the render_report `empty_rounds` parameter do not exist at
    // HEAD — these calls did not compile before the fix landed; order
    // deep-research-t7b, pre-registered).
    // ------------------------------------------------------------------

    /// Every no-evidence round is first-class report content — the
    /// round-level state the verdict assembly had no reader for (the
    /// defect forensics: rounds whose fetches were all dedup-refused
    /// rendered identically to rounds that never added evidence). The
    /// section names every empty round and its reason, and says what
    /// the absence means.
    #[test]
    fn empty_rounds_section_renders_every_empty_round() {
        let claims = final_claims(&[], &window());
        let empty_rounds = [
            EmptyRound {
                round: 2,
                reason: EmptyRoundReason::Refused,
            },
            EmptyRound {
                round: 3,
                reason: EmptyRoundReason::Mixed,
            },
        ];
        let report = render_report("Q", &claims, "run-1", None, None, &[], &empty_rounds);
        assert!(
            report.contains("## No evidence fetched"),
            "the no-evidence section must render: {report}"
        );
        for er in &empty_rounds {
            assert!(
                report.contains(&format!("round {}", er.round)),
                "every no-evidence round must be named in the section: {report}"
            );
            assert!(
                report.contains(er.reason.as_str()),
                "every no-evidence round must name its reason: {report}"
            );
        }
        assert!(
            report.contains("no evidence was added this round"),
            "the section must say what the absence means: {report}"
        );
    }

    /// Empty empty_rounds renders NO section — a run where every round
    /// added evidence has nothing to disclose, and the report must not
    /// grow a vestigial heading (keeps the meridian/reframe/align
    /// goldens byte-pinned).
    #[test]
    fn empty_rounds_empty_renders_no_section() {
        let claims = final_claims(&[], &window());
        let report = render_report("Q", &claims, "run-1", None, None, &[], &[]);
        assert!(
            !report.contains("No evidence fetched"),
            "an empty rounds list must render no section: {report}"
        );
    }

    // ------------------------------------------------------------------
    // T6b pre-window slice — the clean RACE render (RED-FIRST: the
    // render_race stub returned the empty string when this test was
    // written; the assertions watched the red before the body landed —
    // order deep-research-t6b, pre-registered).
    // ------------------------------------------------------------------

    /// The race page leads with passed findings carrying their TYPED
    /// citations (evidence id + URL) and never a bare model-written
    /// tail in [passed] position; downgraded claims are visibly
    /// stamped, never removed; the verdict transcript is untouched.
    #[test]
    fn race_render_leads_with_typed_citations_and_stamps_downgrades() {
        let audits = vec![
            ClaimAudit {
                claim: "The bridge was completed in 1873 [Source: https://example.com/draft]. "
                    .to_string(),
                verdict: Verdict::Passed,
                action: super::super::icd::GateAction::CitationGrounded,
                witness: Default::default(),
                supporting_chunk_ids: vec!["ev-1".to_string()],
                empty_evidence_window: false,
                reason: None,
                corroboration: None,
            },
            ClaimAudit {
                claim: "The engineer was Helena Voss.".to_string(),
                verdict: Verdict::Failed,
                action: super::super::icd::GateAction::AbstainedDecline,
                witness: Default::default(),
                supporting_chunk_ids: Vec::new(),
                empty_evidence_window: false,
                reason: None,
                corroboration: None,
            },
            ClaimAudit {
                claim: "Funding is unclear.".to_string(),
                verdict: Verdict::CouldNotJudge,
                action: super::super::icd::GateAction::RefusedNoCitationHandle,
                witness: Default::default(),
                supporting_chunk_ids: Vec::new(),
                empty_evidence_window: false,
                reason: None,
                corroboration: None,
            },
        ];
        let claims = final_claims(&audits, &window());
        let page = render_race("Meridian Bridge history", &claims, "run-1");
        // The article leads with the question, then the findings.
        assert!(page.starts_with("# Meridian Bridge history"), "{page}");
        assert!(page.contains("## Findings"), "{page}");
        let findings = page
            .split("## Findings")
            .nth(1)
            .expect("findings section present");
        assert!(
            findings.contains("[passed]"),
            "passed renders in [passed] position"
        );
        assert!(
            findings.contains("ev-1"),
            "typed evidence id renders: {findings}"
        );
        assert!(
            findings.contains("https://example.com/a"),
            "typed source URL renders: {findings}"
        );
        assert!(
            !findings.contains("[Source:"),
            "no model-written tail in [passed] position: {findings}"
        );
        // The raw text stays on the verdict-set side (scorer pair
        // formation resolves tails through the draft registry).
        assert_eq!(
            claims[0].text,
            "The bridge was completed in 1873 [Source: https://example.com/draft]. "
        );
        // Downgraded claims are visibly stamped, never removed.
        assert!(page.contains("[refuted]"), "{page}");
        assert!(page.contains("Helena Voss"), "{page}");
        assert!(page.contains("[open question]"), "{page}");
        assert!(page.contains("Funding is unclear"), "{page}");
        assert!(
            !page.contains("[could-not-judge]"),
            "the page uses its own stamps, not the transcript's: {page}"
        );
        // The transcript function is untouched — it still renders its
        // own stamps.
        let report = render_report(
            "Meridian Bridge history",
            &claims,
            "run-1",
            None,
            None,
            &[],
            &[],
        );
        assert!(report.contains("[could-not-judge]"), "{report}");
    }

    // ---- drb1-t5: the composed deliverable's annotations ------------

    fn composed_claim(text: &str, v: Verdict) -> FinalClaim {
        FinalClaim {
            id: "c1".to_string(),
            text: text.to_string(),
            verdict: v,
            status: format!("{v:?}"),
            evidence_ids: Vec::new(),
            citations: Vec::new(),
            flag: None,
            corroboration: None,
        }
    }

    /// A refuted claim must be marked WHERE IT STANDS. Appending it to a
    /// list at the bottom would leave the sentence asserting, in the
    /// body of the page, something the evidence contradicts.
    #[test]
    fn annotate_marks_a_refuted_claim_in_place() {
        let report = "## Findings\n\nThe bridge opened in 1911 and carries six lanes.\n";
        let claims = vec![composed_claim(
            "The bridge opened in 1911 and carries six lanes.",
            Verdict::Failed,
        )];
        let out = annotate_composed(report, &claims);
        let at = out.find("six lanes.").expect("the sentence survives");
        assert!(
            out[at..].starts_with("six lanes. **[refuted by the evidence]**"),
            "the mark sits on the sentence, got: {}",
            &out[at..at.saturating_add(80).min(out.len())]
        );
    }

    /// RED (2026-08-27): the claim carries its INTERNAL handle and the report
    /// carries the RENDERED number — the shape every production run has.
    ///
    /// The test above passes a claim with no citation at all, which is why it
    /// stayed green while production marked NOTHING. Measured on run
    /// dr-1787768404: 192 refuted claims, 192 silently unmarked, 0 located.
    /// A claim is extracted from the pre-render draft ("…six lanes.
    /// [Source: ev-13]"); `number_citations` has already rewritten the report
    /// to "…six lanes. [13]"; the exact `find` misses on every claim that ends
    /// in a citation, which is nearly all of them because the writer is told
    /// to cite as it goes. The page then asserted all 192 as fact, and the
    /// Verification section said they "are marked in place".
    ///
    /// This is §18.1's missing failing input: a guard whose fixture cannot
    /// exhibit the defect is not a guard.
    #[test]
    fn a_refuted_claim_is_marked_even_though_its_citation_was_renumbered() {
        let report = "## Findings\n\nThe bridge opened in 1911 and carries six lanes. [13]\n";
        let claims = vec![composed_claim(
            "The bridge opened in 1911 and carries six lanes. [Source: ev-13]",
            Verdict::Failed,
        )];
        let out = annotate_composed(report, &claims);
        assert!(
            out.contains("six lanes. **[refuted by the evidence]**"),
            "the mark must land despite the renumbering, got: {out}"
        );
        assert!(
            out.contains("1 are marked in place"),
            "and the Verification section must count what it actually marked: {out}"
        );
    }

    /// A refutation that cannot reach the page is REPORTED, never dropped —
    /// the reader is told the statement is contradicted even when the
    /// sentence could not be located to mark.
    #[test]
    fn an_unmarkable_refutation_is_listed_rather_than_lost() {
        let report = "## Findings\n\nSomething else entirely.\n";
        let claims = vec![composed_claim(
            "The bridge opened in 1911 and carries six lanes.",
            Verdict::Failed,
        )];
        let out = annotate_composed(report, &claims);
        assert!(
            out.contains("could not be located in the finished text"),
            "the miss is disclosed: {out}"
        );
        assert!(
            out.contains("The bridge opened in 1911"),
            "and the statement itself is named so the reader can act on it: {out}"
        );
        assert!(
            !out.contains("1 are marked in place"),
            "it must NOT claim a marking it did not make: {out}"
        );
    }

    /// Everything the gate could not verify is NAMED. The article is not
    /// rebuilt around the verdicts, but no unverified claim is passed
    /// off as established either.
    #[test]
    fn annotate_names_every_unverified_claim() {
        let report = "## Findings\n\nAlpha holds. Beta holds.\n";
        let claims = vec![
            composed_claim("Alpha holds.", Verdict::CouldNotJudge),
            composed_claim("Beta holds.", Verdict::Passed),
        ];
        let out = annotate_composed(report, &claims);
        assert!(
            out.contains("## Verification"),
            "the page carries its own audit"
        );
        assert!(
            out.contains("1 verified"),
            "the passed count is stated, got: {out}"
        );
        assert!(
            out.contains("could not be verified"),
            "the unverified count is stated"
        );
        assert!(
            out.contains("- Alpha holds."),
            "the unverified claim is named, not silently kept"
        );
        assert!(
            !out.contains("- Beta holds."),
            "a verified claim is not listed as unverified"
        );
    }

    /// The article survives annotation: composing does not truncate.
    #[test]
    fn annotate_keeps_the_whole_article() {
        let report = "## A\n\nbody one.\n\n## B\n\nbody two.\n";
        let out = annotate_composed(report, &[]);
        assert!(out.starts_with(report), "the article is prefix-preserved");
        assert!(
            out.contains("Of 0 claims"),
            "an empty claim set is still reported"
        );
    }
}
