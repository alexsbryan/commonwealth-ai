// SPDX-License-Identifier: AGPL-3.0-or-later
//! The resolve step — evidence requests into cited chunks.
//!
//! The second half of `EPISTEMIC_INDEX.md` §1's Walk row: "`ground(...)` ->
//! evidence requests, then resolve to chunks". [`super::ground`] decides
//! WHICH ideas the question reaches; this file decides which of their
//! evidence anchors actually become passages, and why the rest did not.
//!
//! It is separate from the walk because the two have different inputs and
//! different failure modes — the walk touches no index and the resolve is
//! nothing but index I/O — and because the walk's callers share every
//! decision here while sharing none of the I/O. That split is the whole
//! design: [`EvidenceFetcher`] is two methods wide, and everything else
//! (scope, budget, title filter, duplicate identity, scoring) is decided
//! once, here, for both callers.

use std::collections::HashSet;

use kernel_types::CorpusId;

use crate::types::ScoredChunk;

use super::context::ChunkRequest;
use super::evidence_site::ChunkSelector;

// ═══════════════════════════════════════════════════════════════════════
// The resolve step
// ═══════════════════════════════════════════════════════════════════════

/// The two fetches a resolve needs, and nothing else.
///
/// Deliberately TWO methods, not a retrieval trait: every other decision
/// between a [`ChunkRequest`] and a chunk — scope, budget, title filter,
/// duplicate, score — is made once in [`resolve_evidence`] and is not the
/// caller's to re-derive. `sovereign-core` implements these over its
/// lane-scoped retrieval pipeline; `corpus-mcp` over a `CorpusIndex` handle.
/// Neither can accidentally implement a different walk.
#[allow(async_fn_in_trait)]
pub trait EvidenceFetcher {
    /// A direct key, no search — [`ChunkSelector::RowId`]. `None` when the
    /// corpus is not open or the row is not present.
    async fn by_row(&self, corpus: &CorpusId, row: u64) -> Option<ScoredChunk>;

    /// A search scoped to one corpus, for [`ChunkSelector::Section`]. The
    /// title filter is applied by the caller of this method, not by it.
    async fn by_search(&self, corpus: &CorpusId, query: &str, limit: usize) -> Vec<ScoredChunk>;
}

/// A chunk the walk brought back, with the ideas that motivated it.
///
/// The atom ids are what turns a passage into a CITED one for a reader: they
/// say which idea node this passage is evidence FOR, which is the link `ask`'s
/// map section needs and which a bare `ScoredChunk` cannot carry.
#[derive(Debug, Clone)]
pub struct ResolvedChunk {
    pub chunk: ScoredChunk,
    /// Atom ids in the walked neighbourhood whose evidence anchors point at
    /// this chunk, highest-weight first (the order `ground` aggregated them).
    pub motivating_atoms: Vec<String>,
    /// The request's aggregate walk score, before the ranking calibration
    /// applied to `chunk.score`.
    pub walk_score: f32,
}

/// Why a request did not become a chunk. Named reasons, because a zero yield
/// that cannot say which zero it is, is what let the SEP defect survive.
#[derive(Debug, Clone, Default)]
pub struct ResolveLedger {
    /// Requests offered.
    pub considered: usize,
    /// Chunks added.
    pub added: usize,
    /// Outside the caller's allow-list.
    pub out_of_scope: usize,
    /// The chunk the atom pointed at could not be fetched.
    pub unresolvable: usize,
    /// Fetched, but no hit carried the article title the site asked for.
    pub title_mismatch: usize,
    /// Already emitted this turn.
    pub duplicate: usize,
}

impl ResolveLedger {
    /// Requests never attempted because the budget was already spent. A
    /// DECISION, not a failure — and the accounting identity
    /// (`considered == added + every reason`) requires it named.
    ///
    /// SATURATING, and the reason is not defensiveness: a
    /// [`ChunkSelector::Section`] request is resolved by a SEARCH, and one
    /// search can return several passages that all pass the title filter, so
    /// `added` counts chunks while `considered` counts requests and the two
    /// are not the same unit. When a few requests realise many chunks, `added`
    /// legitimately exceeds `considered` and the honest remainder is zero, not
    /// a negative number wrapped into `usize::MAX`. (`corpus-mcp`'s `ask`
    /// panicked on exactly that subtraction the first time it was run against
    /// a real atlas.)
    pub fn budget_exhausted(&self) -> usize {
        self.considered.saturating_sub(
            self.added
                + self.out_of_scope
                + self.unresolvable
                + self.title_mismatch
                + self.duplicate,
        )
    }
}

/// Turn evidence requests into chunks.
///
/// `budget` is how many chunks may be ADDED — the row's `budget`. Up to
/// `budget * 2` requests are attempted, so drops do not eat the budget; that
/// 2× headroom is what the pre-policy glue had and removing it would reduce
/// yield on every corpus.
///
/// `allowed` is the caller's corpus allow-list, checked against the corpus
/// the fetch will actually SEARCH ([`EvidenceSite::chunk_corpus`]) so the two
/// cannot disagree — the specific bug `evidence_site` was minted to make
/// unsayable.
pub async fn resolve_evidence<F: EvidenceFetcher>(
    requests: &[ChunkRequest],
    budget: usize,
    allowed: Option<&[String]>,
    fetcher: &F,
) -> (Vec<ResolvedChunk>, ResolveLedger) {
    let mut out = Vec::new();
    let mut ledger = ResolveLedger {
        considered: requests.len(),
        ..Default::default()
    };
    let mut seen: HashSet<String> = HashSet::new();
    for req in requests.iter().take(budget.saturating_mul(2)) {
        if out.len() >= budget {
            break;
        }
        let corpus = req.site.chunk_corpus();
        if let Some(allow) = allowed {
            if !allow.iter().any(|c| c.as_str() == corpus.as_str()) {
                ledger.out_of_scope += 1;
                continue;
            }
        }
        match &req.selector {
            ChunkSelector::RowId(row) => {
                let Some(mut chunk) = fetcher.by_row(corpus, *row).await else {
                    ledger.unresolvable += 1;
                    continue;
                };
                if !seen.insert(dedup_key(&chunk)) {
                    ledger.duplicate += 1;
                    continue;
                }
                score_atlas_chunk(&mut chunk, req);
                out.push(ResolvedChunk {
                    chunk,
                    motivating_atoms: req.motivating_atoms.clone(),
                    walk_score: req.score,
                });
            }
            ChunkSelector::Section(_) => {
                let query = format!("{} {}", req.site.label(), req.passage_preview);
                let hits = fetcher.by_search(corpus, &query, 30).await;
                let mut matched_any = false;
                for hit in hits {
                    if let Some(article) = req.site.article() {
                        if hit.title.as_deref() != Some(article) {
                            continue;
                        }
                    }
                    matched_any = true;
                    if !seen.insert(dedup_key(&hit)) {
                        ledger.duplicate += 1;
                        continue;
                    }
                    let mut chunk = hit;
                    score_atlas_chunk(&mut chunk, req);
                    out.push(ResolvedChunk {
                        chunk,
                        motivating_atoms: req.motivating_atoms.clone(),
                        walk_score: req.score,
                    });
                    if out.len() >= budget {
                        break;
                    }
                }
                if !matched_any {
                    ledger.title_mismatch += 1;
                }
            }
        }
    }
    ledger.added = out.len();
    tracing::debug!(
        target: "retrieval_audit",
        considered = ledger.considered,
        added = ledger.added,
        out_of_scope = ledger.out_of_scope,
        unresolvable = ledger.unresolvable,
        title_mismatch = ledger.title_mismatch,
        duplicate = ledger.duplicate,
        budget_exhausted = ledger.budget_exhausted(),
        "ground: resolve ledger"
    );
    (out, ledger)
}

/// The identity two fetched chunks are the same by. Title plus the head of
/// the content: essence, never a row id, because the same passage reaches the
/// pool by two selectors from two atlases.
fn dedup_key(chunk: &ScoredChunk) -> String {
    let head: String = chunk.content.chars().take(80).collect();
    format!("{}|{}", chunk.title.clone().unwrap_or_default(), head)
}

/// Score an atlas-fetched chunk so it competes with lance-fetched ones, and
/// prepend the verbatim excerpts the motivating atoms carried.
///
/// The `× 0.05` and the `vector_distance` synthesis are the pre-policy glue's
/// numbers, moved rather than re-chosen: they are a RANKING calibration
/// against `cross_corpus_sort_cmp`, not a navigation default, so §2.2 does not
/// govern them and this order does not retune them.
fn score_atlas_chunk(chunk: &mut ScoredChunk, req: &ChunkRequest) {
    chunk.score = req.score * 0.05;
    chunk.vector_distance = Some((1.0_f32 - (req.score / 2.0).min(1.0)).max(0.0));
    if req.verbatim_excerpts.is_empty() {
        return;
    }
    let mut head = String::from("[Atlas highlights]\n");
    for ex in &req.verbatim_excerpts {
        head.push_str(ex);
        head.push('\n');
    }
    head.push('\n');
    head.push_str(&chunk.content);
    chunk.content = head;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resolve budget's accounting identity: considered == added + every
    /// named reason + budget-exhausted. Failing input: drop a reason from
    /// `budget_exhausted`'s subtraction.
    #[test]
    fn the_resolve_ledger_accounts_for_every_request() {
        let l = ResolveLedger {
            considered: 24,
            added: 12,
            out_of_scope: 3,
            unresolvable: 2,
            title_mismatch: 1,
            duplicate: 1,
        };
        assert_eq!(l.budget_exhausted(), 5);
        assert_eq!(
            l.considered,
            l.added
                + l.out_of_scope
                + l.unresolvable
                + l.title_mismatch
                + l.duplicate
                + l.budget_exhausted()
        );
    }

    /// A dedup key is the passage, not its address — the same text fetched
    /// by a row id and by a section slug is ONE chunk.
    #[test]
    fn the_dedup_key_is_the_passage_not_the_row() {
        let mk = |id: u64| ScoredChunk {
            content: "the consequence argument runs as follows".into(),
            title: Some("Free Will".into()),
            url: None,
            corpus_id: "sep".into(),
            score: 1.0,
            metadata: Default::default(),
            chunk_id: Some(id),
            source_doc_id: None,
            vector_distance: None,
            provenance: crate::index::ChunkProvenance::off_the_wire(),
        };
        assert_eq!(dedup_key(&mk(1)), dedup_key(&mk(999)));
    }

    /// A ledger over zero requests is a legitimate zero and accounts for
    /// itself — no request means no drop of any kind.
    #[test]
    fn a_zero_request_resolve_accounts_for_nothing() {
        let l = ResolveLedger::default();
        assert_eq!(l.budget_exhausted(), 0);
    }
}
