// SPDX-License-Identifier: AGPL-3.0-or-later
//! R2 — the estate survey, and the loop's port trait.
//!
//! Estate-first (F16): the estate is asked before any network call, and
//! the survey asserts `estate_searchable` — the loop refuses to open R4
//! while the assert is false. The survey's answer (round 1's draft
//! input) comes from the estate alone: existing-first retrieval, no
//! network before the estate is asked.
//!
//! The port trait is the loop's provider boundary: estate search, web
//! search, web fetch, terminal poll, and the constrained draft. The
//! thin loop in sovereign-core defines the trait; the CLI verb (which
//! depends on corpus-engine + tools-base + sovereign-inference)
//! implements it. The loop never reaches past the trait into any
//! concrete provider.

use super::icd::{
    CorpusEntry, EstatePrecondition, Plan, ReframeInput, Survey, SurveyHit, SurveyQuery,
};
use crate::types::Custody;
use std::path::Path;

/// A term-centered excerpt of a corpus chunk for a hit's snippet —
/// the one implementation shared by every estate surface (the CLI
/// port and the gym's corpus surface). Centers on the deepest query
/// term; falls back to the prefix when no term is present (short
/// chunks, non-lexical matches). Moved here from the CLI verb (t1g
/// rung 2) so the gym's corpus surface uses the SAME snippet shape.
pub fn estate_snippet(content: &str, query: &str, max: usize) -> String {
    // English function words of length >= 4. Small by design: only the
    // words that measurably mis-anchor the window; content terms pass.
    const FUNCTION_WORDS: [&str; 16] = [
        "when", "were", "what", "where", "which", "with", "will", "have", "from", "that", "this",
        "they", "them", "then", "than", "there",
    ];
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .filter(|w| !FUNCTION_WORDS.contains(&w.to_ascii_lowercase().as_str()))
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let lower = content.to_ascii_lowercase();
    let deepest = terms.iter().filter_map(|t| lower.find(t)).max();
    if let Some(center) = deepest {
        // The window ends are raw byte arithmetic on possibly-multibyte
        // content — both must snap to char boundaries or the slice
        // panics (measured 08-17: the DRB Diamond Sutra flight, end 600
        // landed inside 'ā' at 599..601). `center` itself is a boundary
        // (a term match starts on one; to_ascii_lowercase is
        // byte-length-preserving).
        let start = content.floor_char_boundary(center.saturating_sub(200));
        let end = content.ceil_char_boundary((start + max).min(content.len()));
        return content[start..end].to_string();
    }
    content.chars().take(max).collect()
}

/// One search hit as the port returns it. The port stamps custody —
/// code-derived from the source, never model-derived.
#[derive(Debug, Clone)]
pub struct PortHit {
    pub id: String,
    pub url: String,
    pub title: String,
    pub snippet: String,
    /// The hit's BODY as the surface returned it (t1h — the corpus
    /// leg's triage boundary: the snippet is a term-centered 600-char
    /// cut, the body is where the figures live). None on web hits
    /// (additive; the surfaces that have a body fill it).
    pub content: Option<String>,
    pub score: f64,
    /// `estate:<corpus_id>` or `web:<backend_id>` — the origin, recorded.
    pub source: String,
    pub custody: Custody,
}

/// The loop's provider boundary (implemented by the CLI verb).
#[async_trait::async_trait]
pub trait ResearchPort: Send + Sync {
    /// R2 estate listing: corpus metadata for the survey's F16
    /// precondition.
    async fn estate_listing(&self, corpus_ids: &[String]) -> Result<EstateListing, String>;

    /// R2 estate search. `corpus_ids` scopes the estate to the corpora
    /// the survey listed.
    async fn estate_search(
        &self,
        corpus_ids: &[String],
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String>;

    /// R4 web search through the ONE decider (the caller has already
    /// consulted `SpendDecider::allow`).
    async fn web_search(
        &self,
        backend: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<PortHit>, String>;

    /// R6 web fetch (the caller has already consulted `SpendDecider::allow`).
    async fn web_fetch(&self, url: &str) -> Result<String, String>;

    /// F17 terminal-state poll: cheap liveness check on the inference
    /// terminal. Err → the round records every planned fetch as an
    /// absent failure and spends nothing.
    async fn terminal_poll(&self) -> Result<(), String>;

    /// R8 constrained draft: complete `prompt` with the URL constraint
    /// enabled over `allowed_urls` (sovereign-inference
    /// `UrlAllowlistConstraint` at the CLI layer). The model cannot emit
    /// a citation outside the allowed set — structurally.
    async fn draft(
        &self,
        prompt: &str,
        system_message: Option<&str>,
        allowed_urls: &[String],
    ) -> Result<String, String>;

    /// PLAN (order deep-research-t1d fix 2 — breadth): decompose the
    /// question into the acquisition frontier — the sub-question list
    /// the plan records as `queries_preplanned` and the loop's round-1
    /// queries ask (METHODOLOGY.md Plan stage: "the sub-question list
    /// is the search frontier"). The t1c measurement showed the
    /// mechanism this replaces: round 1 asked ONLY the question, so
    /// deck hits whose tokens sit outside the question text never
    /// reached the window (4 of 11 v1 hits).
    ///
    /// The DEFAULT is the deterministic clause split (code, not model):
    /// a port that predates the method, or has no decomposition surface,
    /// still queries a decomposed frontier (which degrades to the whole
    /// question when nothing splits). Ports that CAN generate a
    /// decomposition override this — the CLI delegates a constrained
    /// draft; the mock follows its draft surface (Scripted lines /
    /// Delegated inner port). Model-generated, never silently defaulted
    /// on a live surface: the CLI and mock ALWAYS override.
    async fn plan_subquestions(&self, question: &str) -> Result<Vec<String>, String> {
        Ok(clause_split(question))
    }

    /// STEER 2 (directive 3c5d8b53): the pre-acquisition alignment
    /// decision — shown the plan and its acceptance shapes, the port
    /// decides: proceed, or redirect the question BEFORE any
    /// acquisition spend. The DEFAULT is Proceed (additive: ports that
    /// predate the gate keep running unchanged); a port that wants to
    /// redirect stages `<run_dir>/alignment-input.json` (ReframeInput
    /// shape) and `read_staged_alignment` consumes it.
    async fn alignment_decision(
        &self,
        _plan: &Plan,
        _run_dir: &Path,
    ) -> Result<AlignmentDecision, String> {
        Ok(AlignmentDecision::Proceed)
    }
}

/// The one frontier-size cap — every surface that generates a
/// sub-question decomposition honors it (one decider, one name, §10.6):
/// the default split, the CLI's model decomposition, and the mock's
/// Scripted lines.
pub const FRONTIER_MAX: usize = 12;

/// The deterministic plan_subquestions fallback: split the question on
/// em-dash, semicolon, and ", and " boundaries into fragments (>= 12
/// chars), deduped, capped at FRONTIER_MAX. A question with no such
/// boundaries degrades to itself (the frontier is the question — the
/// pre-fix shape). Code, not model.
pub fn clause_split(question: &str) -> Vec<String> {
    let q = question.trim().trim_end_matches('?').trim();
    let mut parts: Vec<String> = Vec::new();
    for seg in q
        .split(" — ")
        .flat_map(|s| s.split("; "))
        .flat_map(|s| s.split(", and "))
    {
        let seg = seg.trim();
        if seg.chars().count() >= 12 && !parts.contains(&seg.to_string()) {
            parts.push(seg.to_string());
        }
        if parts.len() >= FRONTIER_MAX {
            break;
        }
    }
    parts
}

/// STEER 2: the alignment gate's verdict. `Proceed` — the plan stands,
/// acquisition may begin. `Redirect` — the question is re-planned
/// against the same estate; the redirect is recorded on the run
/// artifacts (alignment-1.json, manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignmentDecision {
    Proceed,
    Redirect { question: String, reason: String },
}

/// STEER 2: read + CONSUME the staged alignment input —
/// `<run_dir>/alignment-input.json` (ReframeInput shape — the
/// question-stewardship sibling of the staged reframe). Absent →
/// `None` (the run aligns: the default Proceed). Present → the
/// operator's Redirect; the file is REMOVED so the redirect fires
/// once — later plans (re-plans) pass without re-prompting. A
/// malformed file refuses loudly (fail-closed, like the reframe
/// input: a truncated redirect must not silently become a proceed).
pub fn read_staged_alignment(run_dir: &Path) -> Result<Option<AlignmentDecision>, String> {
    let path = run_dir.join("alignment-input.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("alignment input unreadable at {path:?}: {e}"))?;
    let input: ReframeInput = serde_json::from_str(&raw)
        .map_err(|e| format!("alignment input malformed at {path:?}: {e}"))?;
    let _ = std::fs::remove_file(&path);
    Ok(Some(AlignmentDecision::Redirect {
        question: input.question,
        reason: input.reason,
    }))
}

/// The estate listing (corpus metadata) as the loop sees it.
#[derive(Debug, Clone)]
pub struct EstateListing {
    pub corpora: Vec<CorpusEntry>,
}

impl EstateListing {
    /// F16 assert: the estate was asked and is searchable. Every LISTED
    /// corpus must be searchable; an empty estate (0 corpora, index
    /// reachable) is searchable — the survey honestly answers "nothing"
    /// rather than refusing, and the loop may open the network leg
    /// (golden fixture, icd-schemas.md §3: `"estate_searchable": true`
    /// with `"corpus-engine index reachable; 0 corpora"`). The refusal
    /// exists for the silent-success trap: a listed-but-unsearchable
    /// estate (missing LanceDB table, foreign embedding space) must not
    /// be skipped while the run looks successful.
    pub fn precondition(&self, detail: &str) -> EstatePrecondition {
        let searchable = self.corpora.iter().all(|c| c.searchable);
        EstatePrecondition {
            asserted: true,
            estate_searchable: searchable,
            detail: detail.to_string(),
        }
    }
}

/// Deterministic survey queries for round 1: the question itself, plus
/// its lead sentence when the question is long (a cheap split, no
/// model). Existing-first: these are the estate's inputs.
pub fn survey_queries(question: &str) -> Vec<String> {
    let mut queries = vec![question.trim().to_string()];
    if question.trim().chars().count() > 200 {
        // The lead clause, up to the first sentence end.
        let lead = question
            .split(|c| c == '.' || c == '?' || c == '!')
            .next()
            .unwrap_or(question)
            .trim()
            .to_string();
        if !lead.is_empty() && lead != question.trim() {
            queries.push(lead);
        }
    }
    queries
}

/// Run the estate survey: ask the estate, record hits, produce the
/// `survey-<round>.json` ICD. The survey answer is the round-1 draft
/// input (estate-first: nothing from the network has been consulted).
pub async fn survey_estate(
    port: &dyn ResearchPort,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    question: &str,
    listing: &EstateListing,
    corpus_ids: &[String],
    max_hits_per_query: usize,
) -> Result<Survey, String> {
    let precondition = listing.precondition(&format!(
        "estate corpora: {} listed, all searchable: {}",
        listing.corpora.len(),
        listing.corpora.iter().all(|c| c.searchable)
    ));
    let mut searched = Vec::new();
    for query in survey_queries(question) {
        let hits = if precondition.estate_searchable {
            port.estate_search(corpus_ids, &query, max_hits_per_query)
                .await?
        } else {
            Vec::new()
        };
        let survey_hits: Vec<SurveyHit> = hits
            .iter()
            .map(|h| SurveyHit {
                chunk_id: h.id.clone(),
                corpus_id: h.source.strip_prefix("estate:").unwrap_or("").to_string(),
                score: h.score,
                url: if h.url.is_empty() {
                    None
                } else {
                    Some(h.url.clone())
                },
                custody: Some(h.custody.as_str().to_string()),
                snippet: h.snippet.clone(),
                // The body rides the survey hit (t1h — the estate
                // window's drafting surface prefers it over the
                // term-centered snippet cut).
                content: h.content.clone(),
            })
            .collect();
        searched.push(SurveyQuery {
            query,
            hits: survey_hits,
        });
    }
    Ok(Survey {
        icd: "survey".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        estate_precondition: precondition,
        estate_corpora: listing.corpora.clone(),
        searched,
        estate_answer: String::new(), // filled by the round's draft pass
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn survey_queries_are_deterministic() {
        let q = "What is known about the Meridian Bridge?";
        let queries = survey_queries(q);
        assert_eq!(queries, vec![q.to_string()]);
        let long = format!("{}. {}", "The Meridian Bridge across the Selune river carries the Bering rail line between Larkhall and Meridian city and was completed in the late nineteenth century after a notorious construction dispute between the two townships, according to county records", "What is known about its engineer?");
        let queries = survey_queries(&long);
        assert_eq!(queries.len(), 2);
        assert!(queries[1].len() < long.len());
    }

    #[test]
    fn precondition_asserts_searchable() {
        let listing = EstateListing {
            corpora: vec![CorpusEntry {
                corpus_id: "meridian".to_string(),
                kind: "documents".to_string(),
                chunks_count: 42,
                searchable: true,
                custody: "personal".to_string(),
            }],
        };
        let p = listing.precondition("test");
        assert!(p.asserted && p.estate_searchable);
        let broken = EstateListing {
            corpora: vec![CorpusEntry {
                corpus_id: "meridian".to_string(),
                kind: "documents".to_string(),
                chunks_count: 42,
                searchable: false,
                custody: "personal".to_string(),
            }],
        };
        let p = broken.precondition("test");
        assert!(p.asserted && !p.estate_searchable);
    }

    /// The golden fixture's empty-estate case (icd-schemas.md §3:
    /// `estate_searchable: true` with "corpus-engine index reachable;
    /// 0 corpora"). A fresh user with no corpora must not be refused —
    /// the survey honestly answers "nothing" and the web leg opens.
    /// Watched failure: the demo run's first measurement caught the
    /// implementation requiring a non-empty estate, refusing a legal
    /// case (run1-console.log, "estate precondition failed (F16)").
    #[test]
    fn precondition_empty_estate_is_searchable() {
        let listing = EstateListing {
            corpora: Vec::new(),
        };
        let p = listing.precondition("corpus-engine index reachable; 0 corpora");
        assert!(
            p.asserted && p.estate_searchable,
            "an empty estate with a reachable index is searchable — \
             the loop must open the network leg, not refuse (golden fixture): {p:?}"
        );
    }

    /// Measured panic (DRB local task 95, 2026-08-17): the deepest term
    /// sits in a multibyte chunk, so the byte-arithmetic window ends
    /// land mid-char and `content[start..end]` panics ("end byte index
    /// 600 is not a char boundary; it is inside 'ā' (bytes 599..601)").
    /// The window must snap to char boundaries on both ends.
    #[test]
    fn estate_snippet_window_snaps_to_char_boundaries() {
        // Case 1 — the exact crash shape: term "prajñāpāramitā" (18
        // bytes) at byte 200, a second 'ā' spanning 599..601 so end=600
        // is mid-char.
        let content = format!(
            "{}prajñāpāramitā{}{}",
            "a".repeat(200),
            "a".repeat(381),
            "ārest"
        );
        assert!(!content.is_char_boundary(600));
        let snippet = estate_snippet(&content, "Prajñāpāramitā", 600);
        assert!(
            snippet.contains("prajñāpāramitā"),
            "snippet must carry the term: {snippet}"
        );
        // Case 2 — the start end lands mid-char: 100 'ā' (200 bytes),
        // term at byte 249, so start=49 is inside the ā spanning 48..50.
        let content = format!(
            "{}a{}prajñāpāramitā{}",
            "ā".repeat(100),
            "a".repeat(49),
            "a".repeat(100)
        );
        assert!(!content.is_char_boundary(49));
        let snippet = estate_snippet(&content, "Prajñāpāramitā", 600);
        assert!(
            snippet.contains("prajñāpāramitā"),
            "snippet must carry the term: {snippet}"
        );
    }
}
