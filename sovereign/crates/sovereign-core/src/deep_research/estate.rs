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

use super::icd::{CorpusEntry, EstatePrecondition, Survey, SurveyHit, SurveyQuery};
use crate::types::Custody;

/// One search hit as the port returns it. The port stamps custody —
/// code-derived from the source, never model-derived.
#[derive(Debug, Clone)]
pub struct PortHit {
    pub id: String,
    pub url: String,
    pub title: String,
    pub snippet: String,
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
}
