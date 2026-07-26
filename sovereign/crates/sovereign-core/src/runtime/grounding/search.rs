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
        // Pinned passages FIRST, and unconditionally — `search_corpus` bails
        // early when there's no engine or no allowed corpus, and a turn-local
        // passage must survive those paths too. They are part of the sealed
        // universe, so including them widens the EVIDENCE, never the scope.
        let mut out = self.pinned.clone();
        out.extend(self.search_corpus(claim).await);
        out
    }
}

/// The attached-asset evidence universe: every embedded chunk of ONE
/// attached document, cosine-ranked per claim. The seal is structural
/// — the searcher is constructed from exactly the asset's chunks (the
/// same set `fetch_quote_verification_surface` reads), so it cannot
/// reach any other document or corpus.
pub(crate) struct AttachedAssetSearcher {
    inference: Arc<dyn InferenceProvider>,
    /// `(content, embedding)` per embedded chunk. Chunks without a
    /// stored embedding are dropped at construction (defensive —
    /// ingest stores embeddings alongside content).
    chunks: Vec<(String, Vec<f32>)>,
}

impl AttachedAssetSearcher {
    pub(crate) fn new(
        inference: Arc<dyn InferenceProvider>,
        chunks: &[crate::types::DocumentChunk],
    ) -> Self {
        Self {
            inference,
            chunks: chunks
                .iter()
                .filter_map(|c| c.embedding.as_ref().map(|e| (c.content.clone(), e.clone())))
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl SealedEvidenceSearch for AttachedAssetSearcher {
    async fn search(&self, claim: &str) -> Vec<String> {
        if self.chunks.is_empty() {
            return Vec::new();
        }
        // Same embed call the attached_doc_search tool uses for its
        // query leg, so claim vectors live in the same space as the
        // stored chunk embeddings.
        let claim_emb = match self.inference.embed(claim).await {
            Ok(e) if !e.is_empty() => e,
            _ => return Vec::new(),
        };
        let mut scored: Vec<(f32, &String)> = self
            .chunks
            .iter()
            .map(|(content, emb)| (crate::memory::cosine_similarity(&claim_emb, emb), content))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(CLAIM_SEARCH_K)
            .map(|(_, content)| content.clone())
            .collect()
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
    /// Sealed passages that exist ONLY in this turn, not in any corpus —
    /// today the code-intel call-graph block. `search_corpus` re-searches the
    /// installed indexes, so a turn-local passage is structurally invisible to
    /// it: the claim audit would re-derive its evidence from the corpus and
    /// find nothing, then flag facts that were sitting verbatim in the sealed
    /// universe. These ride along with every claim's results so the audit sees
    /// the same evidence the synthesis did. Empty on every non-code turn.
    pinned: Vec<String>,
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
            pinned: Vec::new(),
        }
    }
}

/// Turn-local conversation evidence for [`ClaimSearcher::pinned`].
///
/// The claim audit RE-SEARCHES the corpus per claim. A fact the user and
/// assistant established EARLIER IN THIS CONVERSATION lives in no corpus,
/// so it is structurally invisible to that re-search: the audit finds
/// nothing stating it and flags a correctly-recalled fact as ungrounded.
///
/// Measured 2026-07-25 (gap-probe run): asked to recall an element named
/// 26 turns earlier, conversation-history retrieval surfaced the right
/// turn at rank 1 — and the gate still abstained with
/// `violation_prob: 1.0`, because the corpus passages didn't restate it.
/// The user sees "I couldn't confirm an answer against the passages your
/// sources turned up" for something the system remembered perfectly.
///
/// Pinning these widens the EVIDENCE, never the scope — the same
/// contract the code-trace pin already satisfies. These passages are the
/// conversation's own turns; admitting them as evidence for claims about
/// that conversation grants no reach beyond the sealed universe the
/// synthesis prompt already saw.
pub(crate) fn conversation_pinned_evidence(
    context: &crate::types::ConversationContext,
) -> Vec<String> {
    let mut out = Vec::new();
    // Similarity-selected earlier turns (history.rs `maybe_retrieve_relevant_history`).
    if let Some(hits) = context.history_retrieval_hits.as_ref() {
        out.extend(hits.iter().map(|h| h.content.clone()));
    }
    // The compacted preamble standing in for turns that rolled out of
    // the visible window entirely.
    if let Some(preamble) = context.compacted_history.as_deref() {
        out.push(preamble.to_string());
    }
    out
}

/// Citation label carried by sealed conversation turns. Matches the form
/// `CONVERSATION_EVIDENCE_DIRECTIVE` tells the model to cite, so the
/// citation-attribution check reads such a `[Source: …]` as legitimate.
pub(crate) const CONVERSATION_EVIDENCE_LABEL: &str = "earlier in this conversation";

/// Seal this conversation's own turns into a gate EVIDENCE UNIVERSE, the
/// same way the code trace joins it.
///
/// Pinning them on [`ClaimSearcher`] alone is not enough: the pin only
/// widens the per-claim confirmatory loop, while the entity-anchored
/// value-presence check (`value_presence::assess_asserted_value`) tests
/// the answer's asserted value against `EvidenceContext::chunks` and
/// returns `vp = 1.0` outright when it is absent — short-circuiting
/// before any widening runs. Measured 2026-07-26: with the synthesis
/// prompt fixed so the model DOES answer "polonium" from the recalled
/// turn, value-presence found no "polonium" among the corpus chunks and
/// hard-abstained the turn (vp 1.0), replacing a correct answer with
/// "I couldn't confirm an answer …". Sealing widens the evidence, never
/// the scope — these passages are turns the synthesis prompt already saw.
///
/// `chunk_labels` is PARALLEL to `chunks`; it is extended only while that
/// invariant holds, matching the code-trace site's guard.
pub(crate) fn seal_conversation_evidence(
    context: &crate::types::ConversationContext,
    chunks: &mut Vec<String>,
    source_labels: &mut Vec<String>,
    chunk_labels: &mut Vec<Vec<String>>,
) -> usize {
    let turns = conversation_pinned_evidence(context);
    if turns.is_empty() {
        return 0;
    }
    let sealed = turns.len();
    for turn in turns {
        chunks.push(turn);
        if chunk_labels.len() + 1 == chunks.len() {
            chunk_labels.push(vec![CONVERSATION_EVIDENCE_LABEL.to_string()]);
        }
    }
    source_labels.push(CONVERSATION_EVIDENCE_LABEL.to_string());
    tracing::info!(
        target: "grounding_gate",
        sealed_turns = sealed,
        evidence_chunks = chunks.len(),
        "sealed conversation turns into the gate's evidence universe"
    );
    sealed
}

impl ClaimSearcher {
    /// Attach turn-local sealed passages (see [`ClaimSearcher::pinned`]).
    pub(crate) fn with_pinned(mut self, pinned: Vec<String>) -> Self {
        self.pinned = pinned
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        self
    }

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
