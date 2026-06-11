// SPDX-License-Identifier: AGPL-3.0-or-later
//! Claim-conditioned search inside a sealed evidence universe.
//!
//! `SealedEvidenceSearch` is the one-method capability the gate's
//! audit uses to widen verification from the prompt snapshot to the
//! turn's sealed evidence universe. Contract (every impl): widen the
//! EVIDENCE, never the scope; return empty on ANY failure — the audit
//! degrades to the prompt snapshot, never aborts, never worse than
//! pre-feature behavior.

use std::collections::HashSet;
use std::sync::Arc;

use crate::traits::InferenceProvider;

use super::super::Runtime;

/// Claim-conditioned search inside ONE sealed evidence universe.
#[async_trait::async_trait]
pub(crate) trait SealedEvidenceSearch: Send + Sync {
    async fn search(&self, claim: &str) -> Vec<String>;
}

#[async_trait::async_trait]
impl SealedEvidenceSearch for ClaimSearcher {
    async fn search(&self, claim: &str) -> Vec<String> {
        self.search_corpus(claim).await
    }
}

/// Per-claim hits fed to the joint judge and (for failed claims) the
/// rewrite. Small: one embed + one sealed hybrid search per claim.
const CLAIM_SEARCH_K: usize = 4;

/// Per-claim corpus retrieval for the long-form audit.
///
/// The audit's original evidence universe was *the chunks that
/// happened to be retrieved for the question* — measured dead end
/// (v13c/v14/v14b, 2026-06-11): on broad "maximal" questions the
/// prompt set covers a corner of the corpus, so TRUE claims the
/// corpus does state fail verification, the rewrite has nothing to
/// correct them WITH, and the essay collapses into disclaimers.
/// This searcher widens verification to *the sealed corpus*: each
/// audited claim is embedded and searched directly (hybrid
/// vector+FTS), claim-conditioned hits join the judge's passage set,
/// and failed claims' hits are handed to the rewrite so it can
/// REPLACE a wrong assertion with what the corpus actually states.
/// Confabulated claims retrieve nothing that states them — the
/// safety property of the audit is unchanged.
///
/// Built by each gate call site from cloneable Runtime parts:
/// `gate_answer` runs inside spawned stream tasks that hold no
/// `&Runtime` (see "no borrows of self" in streaming.rs).
#[derive(Clone)]
pub(crate) struct ClaimSearcher {
    inference: Arc<dyn InferenceProvider>,
    engine: Option<Arc<corpus_engine::CorpusEngine>>,
    /// HARD seal: claim search may only read these corpora — exact
    /// id match, layer corpora of an allowed parent are NOT retained
    /// (sealed errs restrictive, same contract as the round-2 mesh
    /// seal in evidence_loop.rs).
    allowed_corpora: Vec<String>,
}

impl Runtime {
    /// Build the gate's claim searcher for one turn. The seal is the
    /// conversation's explicit corpus allow-list when present, else
    /// the corpora round-0 retrieval actually drew from — claim
    /// search must widen the EVIDENCE, never the corpus scope (the
    /// agentic round-2 contract).
    pub(crate) fn claim_searcher(
        &self,
        enabled_corpora: Option<&[String]>,
        chunks: &[corpus_engine::ScoredChunk],
    ) -> ClaimSearcher {
        let allowed: Vec<String> = match enabled_corpora {
            Some(ids) if !ids.is_empty() => ids.to_vec(),
            _ => {
                let mut ids: Vec<String> = chunks
                    .iter()
                    .map(|c| c.corpus_id.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                ids.sort();
                ids
            }
        };
        ClaimSearcher {
            inference: Arc::clone(&self.inference),
            engine: self.corpus_engine.clone(),
            allowed_corpora: allowed,
        }
    }
}

impl ClaimSearcher {
    /// Sealed hybrid search for ONE audited claim. Returns chunk
    /// contents, interleaved per corpus (cross-corpus scores don't
    /// compose — `ScoredChunk`'s own caveat), capped at
    /// `CLAIM_SEARCH_K`. Empty on any failure: the audit then judges
    /// against the prompt chunks alone — exactly the pre-feature
    /// behavior, never worse.
    pub(crate) async fn search_corpus(&self, claim: &str) -> Vec<String> {
        let Some(engine) = &self.engine else {
            return Vec::new();
        };
        if self.allowed_corpora.is_empty() {
            return Vec::new();
        }
        // Hybrid search tolerates an empty vector leg (FTS still
        // runs), so an embed failure degrades rather than aborts.
        let embedding = self.inference.embed_query(claim).await.unwrap_or_default();
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(
                    target: "grounding_gate",
                    error = %e,
                    "claim search: installed_indexes() failed — auditing against prompt chunks only"
                );
                return Vec::new();
            }
        };
        let mut per_corpus: Vec<Vec<corpus_engine::ScoredChunk>> = Vec::new();
        for info in indexes {
            if !matches!(
                info.kind,
                corpus_engine::CorpusKind::Knowledge | corpus_engine::CorpusKind::Catalog
            ) {
                continue;
            }
            if !self.allowed_corpora.contains(&info.corpus_id) {
                continue;
            }
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(
                        target: "grounding_gate",
                        corpus = %info.corpus_id,
                        error = %e,
                        "claim search: open_index failed"
                    );
                    continue;
                }
            };
            match idx.search(&embedding, claim, CLAIM_SEARCH_K).await {
                Ok(scored) if !scored.is_empty() => per_corpus.push(scored),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "grounding_gate",
                        corpus = %info.corpus_id,
                        error = %e,
                        "claim search: search failed"
                    );
                }
            }
        }
        // Round-robin across corpora up to the cap.
        let mut out: Vec<String> = Vec::new();
        let mut rank = 0usize;
        while out.len() < CLAIM_SEARCH_K {
            let mut any = false;
            for corpus_hits in &per_corpus {
                if let Some(c) = corpus_hits.get(rank) {
                    any = true;
                    if out.len() < CLAIM_SEARCH_K {
                        out.push(c.content.clone());
                    }
                }
            }
            if !any {
                break;
            }
            rank += 1;
        }
        out
    }
}
