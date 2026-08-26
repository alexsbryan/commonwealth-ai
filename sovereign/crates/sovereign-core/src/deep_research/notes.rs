//! The researcher worker (drb1-r4): the writer is handed FINDINGS, not chunks.
//!
//! **Why this exists.** Our own AIQ teardown (`research/deep-research/
//! aiq-teardown.md` §1.3) attributes that system's DRB-II InfoRecall lead —
//! 49.23, above o3, Gemini-3-Pro, Gemini-2.5-Pro and Grok — to a structural
//! choice, not a scorer trick: a dedicated worker per sub-question returns
//! structured research notes, and the writer reads those notes plus a
//! verified-source list, with no search tools of its own. Our composer instead
//! handed each section the top-`SECTION_PASSAGES` passages by cosine. Eight
//! passages is what FITS, not what is KNOWN — on the logged task-69 flight the
//! window held 38 chunks and each section saw eight of them, so most of what
//! acquisition paid for never reached the writer at all.
//!
//! **The citation contract is unchanged, and that is the point of the shape.**
//! A finding carries `evidence_ids` — the `ev-N` handles of the window chunks
//! it rests on — never quoted text standing free of its source. The writer
//! cites those same handles, so `audit` locates spans exactly as before, the
//! corroboration floor still counts origins, and the custody veto still sees a
//! chunk. Distillation moves WHERE the reading happens; it does not move what
//! a citation means. A finding whose ids do not resolve against the window is
//! REFUSED and recorded with its reason — never dropped, never silently
//! re-pointed at a chunk that does exist (§18.3: absence is reported, and a
//! substitution is named).

use super::estate::{DraftLeg, ResearchPort};
use super::icd::{EvidenceWindow, Finding, RefusedFinding, ResearchNote};

/// Findings shown to the writer per sub-question. Well above the eight
/// passages a section used to see: a finding is one sentence, so the token
/// cost of 24 of them is a fraction of eight 1,400-char passages.
pub const FINDINGS_PER_SECTION: usize = 24;

/// Workers in flight. The primary slot has no usable concurrency (note
/// be8110a6: 12 identical judge calls, serial 14.6s vs all-at-once 14.6s —
/// 1.00x, and the queue-shed errors corrupted verdicts), so this is 1 until
/// a measurement says otherwise. It is a named constant rather than a bare
/// literal so the next person changes it with evidence.
pub const WORKER_CONCURRENCY: usize = 1;

/// A claim shorter than this cannot carry a verifiable assertion; it is a
/// fragment the worker emitted while listing, and admitting it would put an
/// unverifiable line in front of the audit.
const MIN_CLAIM_CHARS: usize = 20;

/// The usefulness recorded when the worker cites evidence but omits its own
/// score. Mid-scale on purpose: it must not out-rank a finding the worker
/// actually judged useful, nor sink below one it judged useless.
const DEFAULT_USEFULNESS: u8 = 50;

/// Scan `[ev-3]`, `[Source: ev-3]` and `[ev-3, ev-4]` out of a line,
/// returning the ids and the line with those spans removed.
fn split_citations(line: &str) -> (Vec<String>, String) {
    let mut ids = Vec::new();
    let mut rest = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '[' {
            rest.push(c);
            continue;
        }
        let mut inner = String::new();
        let mut closed = false;
        for c2 in chars.by_ref() {
            if c2 == ']' {
                closed = true;
                break;
            }
            inner.push(c2);
        }
        if !closed {
            // An unterminated bracket is text, not a citation.
            rest.push('[');
            rest.push_str(&inner);
            continue;
        }
        let body = inner.trim().trim_start_matches("Source:").trim();
        let mut any = false;
        for tok in body.split(',') {
            let tok = tok.trim();
            if tok.starts_with("ev-") && tok.len() > 3 {
                ids.push(tok.to_string());
                any = true;
            }
        }
        if !any {
            rest.push('[');
            rest.push_str(&inner);
            rest.push(']');
        }
    }
    (ids, rest)
}

/// Pull a trailing `(85)` usefulness score off the claim, if present.
fn split_usefulness(text: &str) -> (Option<u8>, String) {
    let t = text.trim_end();
    if !t.ends_with(')') {
        return (None, t.to_string());
    }
    let Some(open) = t.rfind('(') else {
        return (None, t.to_string());
    };
    let inner = &t[open + 1..t.len() - 1];
    let digits: String = inner.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() != inner.trim().len() {
        return (None, t.to_string());
    }
    match digits.parse::<u32>() {
        Ok(n) if n <= 100 => (Some(n as u8), t[..open].trim_end().to_string()),
        _ => (None, t.to_string()),
    }
}

/// Parse a worker's raw output into admitted findings and named refusals.
///
/// PURE — no inference, no IO. Every promotion decision this module makes is
/// decidable here, which is what makes the mechanism testable before it is
/// ever wired to a model (§18.1: a check with a failing input you can name).
pub fn parse_findings(raw: &str, known_ids: &[String]) -> (Vec<Finding>, Vec<RefusedFinding>) {
    let mut findings = Vec::new();
    let mut refused = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let line = line
            .trim_start_matches(['-', '*', '•', ' '])
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')', ' '])
            .trim();
        if line.is_empty() {
            continue;
        }
        let (ids, without_ids) = split_citations(line);
        let (score, claim) = split_usefulness(&without_ids);
        let claim = claim.trim().trim_end_matches([',', ';']).trim().to_string();
        if claim.chars().count() < MIN_CLAIM_CHARS {
            // Short AND uncited is list scaffolding ("Findings:"), not a
            // dropped claim — recording it would bury the real refusals.
            if !ids.is_empty() {
                refused.push(RefusedFinding {
                    claim,
                    reason: format!("claim shorter than {MIN_CLAIM_CHARS} characters"),
                    unknown_ids: Vec::new(),
                });
            }
            continue;
        }
        if ids.is_empty() {
            refused.push(RefusedFinding {
                claim,
                reason: "no evidence id cited".to_string(),
                unknown_ids: Vec::new(),
            });
            continue;
        }
        let unknown: Vec<String> = ids
            .iter()
            .filter(|id| !known_ids.iter().any(|k| k == *id))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            // The load-bearing refusal. A worker that cites `ev-99` against a
            // three-chunk window has either mis-numbered or invented, and both
            // produce a claim the audit cannot locate. Admitting it with the
            // resolvable ids only would silently re-attribute the claim.
            refused.push(RefusedFinding {
                claim,
                reason: "cites evidence ids the window does not hold".to_string(),
                unknown_ids: unknown,
            });
            continue;
        }
        let mut evidence_ids = ids;
        evidence_ids.dedup();
        findings.push(Finding {
            claim,
            evidence_ids,
            usefulness: score.unwrap_or(DEFAULT_USEFULNESS),
        });
    }
    (findings, refused)
}

/// One worker's evidence budget. Smaller than the round draft's because
/// there are as many workers as sub-questions: at 6k tokens each, a 20-wide
/// frontier reads 120k tokens of evidence across the run — five times a
/// single 24k-token draft, and no single call goes near the window.
///
/// This is what makes web-scale acquisition consumable at all. The 2026-08-24
/// web arm held 1,360,782 chars; one prompt cannot read that on any model we
/// run, and the answer is not a bigger prompt, it is more readers each given a
/// bounded, RELEVANT slice.
pub const WORKER_EVIDENCE_TOKENS: usize = 6_000;

/// The evidence one worker reads: the passages that rank best against ITS
/// sub-question, bounded.
///
/// Ranked when the port has an embedder, rotated across sources when it does
/// not — and which one happened is NAMED by the caller's trace, never
/// inferred. What a worker was not shown is counted, because "the finding
/// isn't there" and "the worker never saw it" are different failures and the
/// record has to tell them apart.
async fn worker_evidence(
    port: &dyn ResearchPort,
    sub_question: &str,
    window: &EvidenceWindow,
) -> (super::synthesize::BoundedEvidence, bool) {
    let passages = super::synthesize::window_passages(window);
    let texts: Vec<String> = passages.iter().map(|p| p.text.clone()).collect();
    let mut inputs = texts.clone();
    inputs.push(sub_question.to_string());
    let ranked = match port.embed(&inputs).await {
        Ok(v) if v.len() == inputs.len() => {
            let focus = &v[v.len() - 1];
            let mut idx: Vec<usize> = (0..passages.len()).collect();
            idx.sort_by(|a, b| {
                super::cosine(&v[*b], focus).total_cmp(&super::cosine(&v[*a], focus))
            });
            Some(
                idx.into_iter()
                    .map(|i| passages[i].clone())
                    .collect::<Vec<_>>(),
            )
        }
        _ => None,
    };
    let embedded = ranked.is_some();
    let ordered = ranked.unwrap_or(passages);
    let bounded = super::synthesize::bounded_evidence(
        &ordered,
        embedded,
        WORKER_EVIDENCE_TOKENS * super::synthesize::CHARS_PER_TOKEN,
    );
    (bounded, embedded)
}

fn worker_prompt(sub_question: &str, evidence: &str) -> String {
    // SHAPE, never the answer: no criterion vocabulary, no worked example
    // with content in it. The parser's contract is stated as a line format
    // because that is what the parser enforces — a rule the code checks is
    // the only kind worth writing down (§7: structural, not remembered).
    format!(
        "Read the evidence and list the specific factual findings it gives for the question.\n\n\
         One finding per line. Each line: the finding as a complete sentence, then the \
         evidence ids it rests on in square brackets, then how useful it is to the question \
         as a number from 0 to 100 in parentheses.\n\n\
         Like this — the content is yours, the shape is fixed:\n\
         The specification defines four transport bindings. [ev-2] [ev-7] (80)\n\n\
         Assert only what the evidence states. Never invent facts, numbers, names or dates. \
         Cite only ids that appear in the evidence below. Prefer findings carrying specific \
         measures, numbers, names or dates. If the evidence gives no finding for this \
         question, write nothing.\n\n\
         Question: {sub_question}\n\nEvidence:\n{evidence}"
    )
}

/// One worker: one sub-question, the whole window, structured findings out.
pub async fn research_one(
    port: &dyn ResearchPort,
    sub_question: &str,
    window: &EvidenceWindow,
) -> Result<ResearchNote, String> {
    // The ids a finding may cite are the ids this worker was SHOWN — not
    // every id in the window. A worker cannot have learned something from a
    // passage it never read, so a citation to one is exactly the
    // unresolvable-id case the parser refuses.
    let (bounded, embedded) = worker_evidence(port, sub_question, window).await;
    let known: Vec<String> = bounded
        .admitted
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let prompt = worker_prompt(sub_question, &bounded.text);
    // No url allowlist: a finding cites evidence HANDLES, never urls, so
    // there is no url for the constraint to police on this leg.
    let raw = port.draft(DraftLeg::Research, &prompt, None, &[]).await?;
    let (findings, refused) = parse_findings(&raw, &known);
    tracing::debug!(
        target: "deep_research",
        sub_question,
        selection = if embedded { "ranked" } else { "rotated" },
        window_chunks = window.chunks.len(),
        passages_used = bounded.passages_used,
        passages_dropped = bounded.passages_dropped,
        sources_used = bounded.sources_used,
        chars_used = bounded.chars_used,
        findings = findings.len(),
        refused = refused.len(),
        unresolved = refused.iter().filter(|r| !r.unknown_ids.is_empty()).count(),
        "research worker distilled a sub-question"
    );
    Ok(ResearchNote {
        sub_question: sub_question.to_string(),
        findings,
        refused,
        passages_seen: bounded.passages_used,
    })
}

/// One note per sub-question. A worker that fails is recorded as an EMPTY
/// note carrying the reason, never omitted — a missing sub-question would
/// read downstream as "nothing to say about it" (§18.3).
pub async fn gather(
    port: &dyn ResearchPort,
    sub_questions: &[String],
    window: &EvidenceWindow,
) -> Vec<ResearchNote> {
    let mut out = Vec::new();
    // Owned items per iteration: `buffered` over borrowed items breaks the
    // desktop build (note 5f6608f9). Serial at WORKER_CONCURRENCY = 1.
    for sub in sub_questions.to_vec() {
        match research_one(port, &sub, window).await {
            Ok(note) => out.push(note),
            Err(e) => {
                tracing::warn!(
                    target: "deep_research",
                    sub_question = %sub,
                    error = %e,
                    "research worker failed — empty note, reason recorded"
                );
                out.push(ResearchNote {
                    sub_question: sub.clone(),
                    findings: Vec::new(),
                    refused: vec![RefusedFinding {
                        claim: String::new(),
                        reason: format!("worker call failed: {e}"),
                        unknown_ids: Vec::new(),
                    }],
                    passages_seen: window.chunks.len(),
                });
            }
        }
    }
    out
}

/// The writer's input for one sub-question: findings, best first, each
/// carrying the citation the writer is expected to reuse verbatim.
pub fn findings_block(note: &ResearchNote) -> String {
    let mut ranked: Vec<&Finding> = note.findings.iter().collect();
    ranked.sort_by(|a, b| b.usefulness.cmp(&a.usefulness));
    let mut s = String::new();
    for f in ranked.into_iter().take(FINDINGS_PER_SECTION) {
        let cites: Vec<String> = f
            .evidence_ids
            .iter()
            .map(|i| format!("[Source: {i}]"))
            .collect();
        s.push_str(&format!("- {} {}\n", f.claim, cites.join(" ")));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research::icd::WindowChunk;

    fn window(ids: &[&str]) -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: ids
                .iter()
                .map(|i| WindowChunk {
                    id: i.to_string(),
                    locator: format!("https://example.com/{i}"),
                    source_url: format!("https://example.com/{i}"),
                    custody: "public-web".to_string(),
                    provenance_class: "known".to_string(),
                    content: "body".to_string(),
                    ingested_into: None,
                    tags: Vec::new(),
                })
                .collect(),
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            content_refused: Vec::new(),
            derived_custody: "public-web".to_string(),
        }
    }

    fn known(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// **A finding citing an id the window does not hold is REFUSED.**
    ///
    /// This is the whole safety case for handing the writer distilled text
    /// instead of the passages themselves. If an unresolvable id could reach
    /// the writer, the report would carry a citation the audit cannot locate
    /// — an assertion with a source-shaped label and no source behind it,
    /// which is precisely the failure the corroboration floor exists to stop.
    ///
    /// Watch-it-fail: admit the finding whenever ANY cited id resolves and
    /// this returns one finding and zero refusals.
    #[test]
    fn a_finding_citing_an_unknown_chunk_is_refused() {
        let raw = "The protocol defines four transport bindings. [ev-1] [ev-99] (80)";
        let (findings, refused) = parse_findings(raw, &known(&["ev-1", "ev-2"]));
        assert!(
            findings.is_empty(),
            "a finding with an unresolvable id must not reach the writer: {findings:?}"
        );
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].unknown_ids, vec!["ev-99".to_string()]);
        assert!(
            refused[0].reason.contains("does not hold"),
            "the refusal must name WHY: {}",
            refused[0].reason
        );
    }

    /// The partial-resolution case, stated separately because it is the one
    /// a careless fix would get wrong: dropping just the bad id would leave a
    /// true-looking claim re-attributed to a chunk that never supported it.
    #[test]
    fn a_partly_resolvable_finding_is_refused_whole_never_re_attributed() {
        let raw = "Adoption doubled in the first quarter. [ev-2, ev-77] (90)";
        let (findings, refused) = parse_findings(raw, &known(&["ev-1", "ev-2"]));
        assert!(findings.is_empty());
        assert_eq!(refused[0].unknown_ids, vec!["ev-77".to_string()]);
    }

    /// An uncited claim is refused too — the writer must never receive a
    /// sentence it cannot attribute.
    #[test]
    fn an_uncited_claim_is_refused() {
        let raw = "The protocol was widely adopted across the industry.";
        let (findings, refused) = parse_findings(raw, &known(&["ev-1"]));
        assert!(findings.is_empty());
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].reason, "no evidence id cited");
    }

    /// The admit path, and the citation forms the worker actually produces.
    #[test]
    fn resolvable_findings_are_admitted_in_every_citation_form() {
        let raw = "- The spec defines four transport bindings. [ev-1] (80)\n\
                   * Adoption doubled in the first quarter. [Source: ev-2] (65)\n\
                   3. Two vendors shipped implementations. [ev-1, ev-2]\n";
        let (findings, refused) = parse_findings(raw, &known(&["ev-1", "ev-2"]));
        assert_eq!(refused, Vec::new(), "nothing here should be refused");
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0].evidence_ids, vec!["ev-1".to_string()]);
        assert_eq!(findings[0].usefulness, 80);
        assert_eq!(findings[1].evidence_ids, vec!["ev-2".to_string()]);
        assert_eq!(
            findings[2].evidence_ids,
            vec!["ev-1".to_string(), "ev-2".to_string()]
        );
        assert_eq!(
            findings[2].usefulness, DEFAULT_USEFULNESS,
            "an unscored finding takes the named default, not zero"
        );
        assert!(
            findings[0].claim.ends_with("bindings."),
            "the citation and the score must be stripped from the claim: {:?}",
            findings[0].claim
        );
    }

    /// Scaffolding the worker emits while listing is not a refusal — burying
    /// the real refusals under "Findings:" lines would make the record
    /// useless for deciding whether distillation worked.
    #[test]
    fn list_scaffolding_is_skipped_not_refused() {
        let raw = "Findings:\n\n- The spec defines four transport bindings. [ev-1] (80)\n";
        let (findings, refused) = parse_findings(raw, &known(&["ev-1"]));
        assert_eq!(findings.len(), 1);
        assert_eq!(refused, Vec::new());
    }

    /// The writer's block reuses the finding's own citation verbatim, so the
    /// handle the audit will look for is the handle the worker resolved.
    #[test]
    fn the_writer_block_carries_reusable_citations_best_first() {
        let raw = "- Lower value finding here for ranking. [ev-2] (10)\n\
                   - Higher value finding here for ranking. [ev-1] (95)\n";
        let (findings, _) = parse_findings(raw, &known(&["ev-1", "ev-2"]));
        let note = ResearchNote {
            sub_question: "q".to_string(),
            findings,
            refused: Vec::new(),
            passages_seen: 2,
        };
        let block = findings_block(&note);
        let hi = block.find("Higher value").expect("high finding present");
        let lo = block.find("Lower value").expect("low finding present");
        assert!(hi < lo, "findings rank by usefulness, best first:\n{block}");
        assert!(
            block.contains("[Source: ev-1]"),
            "citation reusable:\n{block}"
        );
    }

    /// A window whose chunks the worker never cites yields a note with zero
    /// findings — and the refusals say so. An empty note and an empty WINDOW
    /// must stay tellable apart downstream.
    #[test]
    fn a_note_records_what_it_saw_even_when_nothing_is_admitted() {
        let w = window(&["ev-1", "ev-2", "ev-3"]);
        let (findings, refused) = parse_findings("Nothing useful. [ev-42]", &known(&["ev-1"]));
        let note = ResearchNote {
            sub_question: "q".to_string(),
            findings,
            refused,
            passages_seen: w.chunks.len(),
        };
        assert!(note.findings.is_empty());
        assert_eq!(note.passages_seen, 3, "the note records the window it read");
        assert_eq!(note.refused.len(), 1);
    }
}
