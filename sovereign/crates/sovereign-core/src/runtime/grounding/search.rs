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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::traits::InferenceProvider;

use super::super::Runtime;

/// What `ClaimSearcher` needs from an index store: list what is usable, open
/// one. Two methods, deliberately — this is the seam that lets the six
/// decisions inside `search_corpus` (kill-switch, kind filter, allow-list
/// seal, permit ordering, the concurrency bound, the round-robin cap) be
/// tested against a fake instead of a real `CorpusEngine` with real indexes
/// (ARCH §5.3, §12.2). Production has one implementor, `CorpusEngine`.
#[async_trait::async_trait]
pub(crate) trait SealedIndexSource: Send + Sync {
    async fn usable(&self) -> corpus_engine::Result<Vec<SealedIndexRef>>;
    async fn open(&self, path: &Path) -> corpus_engine::Result<Arc<dyn SealedIndex>>;
}

/// One opened index: hybrid search, contents only — the searcher never reads
/// anything else off a hit.
#[async_trait::async_trait]
pub(crate) trait SealedIndex: Send + Sync {
    async fn search(
        &self,
        embedding: &[f32],
        query: &str,
        k: usize,
    ) -> corpus_engine::Result<Vec<String>>;
}

/// The three facts about an index the searcher decides on.
#[derive(Debug, Clone)]
pub(crate) struct SealedIndexRef {
    pub corpus_id: String,
    pub kind: corpus_engine::CorpusKind,
    pub path: PathBuf,
}

#[async_trait::async_trait]
impl SealedIndexSource for corpus_engine::CorpusEngine {
    async fn usable(&self) -> corpus_engine::Result<Vec<SealedIndexRef>> {
        Ok(self
            .usable_indexes()
            .await?
            .into_iter()
            .map(|i| SealedIndexRef {
                corpus_id: i.corpus_id,
                kind: i.kind,
                path: i.path,
            })
            .collect())
    }

    async fn open(&self, path: &Path) -> corpus_engine::Result<Arc<dyn SealedIndex>> {
        Ok(Arc::new(self.open_index(path).await?))
    }
}

#[async_trait::async_trait]
impl SealedIndex for corpus_engine::CorpusIndex {
    async fn search(
        &self,
        embedding: &[f32],
        query: &str,
        k: usize,
    ) -> corpus_engine::Result<Vec<String>> {
        Ok(
            corpus_engine::CorpusIndex::search(self, embedding, query, k)
                .await?
                .into_iter()
                .map(|c| c.content)
                .collect(),
        )
    }
}

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
    engine: Option<Arc<dyn SealedIndexSource>>,
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
            engine: self
                .corpus_engine
                .clone()
                .map(|e| e as Arc<dyn SealedIndexSource>),
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
    /// A searcher over an arbitrary index source — the test seam.
    #[cfg(test)]
    pub(crate) fn over(
        inference: Arc<dyn InferenceProvider>,
        source: Arc<dyn SealedIndexSource>,
        allowed_corpora: Vec<String>,
    ) -> Self {
        Self {
            inference,
            engine: Some(source),
            allowed_corpora,
            pinned: Vec::new(),
        }
    }

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
        // Cost/value kill-switch — see `config::claim_search_enabled`. OFF
        // degrades to the documented no-searcher behavior (judge against the
        // prompt chunks alone), never to something new.
        if !crate::runtime::grounding::config::claim_search_enabled() {
            tracing::debug!(
                target: "grounding_gate",
                "claim search: DISABLED by SOVEREIGN_GATE_CLAIM_SEARCH — auditing against prompt chunks alone"
            );
            return Vec::new();
        }
        let t_claim = std::time::Instant::now();
        // Hybrid search tolerates an empty vector leg (FTS still
        // runs), so an embed failure degrades rather than aborts.
        let embedding = self.inference.embed_query(claim).await.unwrap_or_default();
        let indexes = match engine.usable().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(
                    target: "grounding_gate",
                    error = %e,
                    "claim search: usable() failed — auditing against prompt chunks only"
                );
                return Vec::new();
            }
        };
        // Carries the corpus id and per-corpus wall time alongside the hits so
        // the audit event below can attribute cost AND yield per corpus. The
        // iteration order is unchanged, so the round-robin's output is
        // byte-identical to the uninstrumented path.
        //
        // ONE CLAIM, EVERY ALLOWED CORPUS, CONCURRENTLY (issue #57). This was a
        // serial loop, which made the gate's fan-out `claims x corpora`
        // sequential round trips — and on an UNSCOPED turn `corpora` is every
        // installed knowledge index, not the one the question named. That is
        // the shape behind `config::claim_search_enabled`'s own note: 753
        // searches inside one gate window costing 608.9 s.
        //
        // `buffered`, NOT `buffer_unordered`: the round-robin below interleaves
        // per corpus and the doc comment above promises "the iteration order is
        // unchanged", so the output must stay byte-identical. `buffered`
        // preserves input order while still running `concurrency` at a time,
        // which makes this purely a wall-time change. Futures are built in a
        // sync loop so each captures only owned values (no fn-scope borrow held
        // across an await, which would trip the Send bound).
        use futures::StreamExt as _;
        let mut tasks = Vec::new();
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
            let engine = Arc::clone(engine);
            let embedding = embedding.clone();
            let claim = claim.to_string();
            let corpus_id = info.corpus_id.clone();
            let path = info.path.clone();
            tasks.push(async move {
                // The one bound, taken at the innermost point so the nested
                // fan-out cannot multiply into it — and taken BEFORE
                // `open_index`, not just around the search. Every task in this
                // loop opens a DIFFERENT corpus, so the engine's index cache
                // cannot dedupe them: a cold fan-out pays `CorpusIndex::open`
                // plus the `info()` chunk-count/dir-size walk concurrently,
                // once per corpus. Opening is half of what this bound exists
                // for — `claim_search_permits` prices the event it was written
                // after as "sixteen concurrent `open_index` + hybrid searches"
                // — so leaving the open outside it bounded only the half that
                // was already cheap. Held across open AND search.
                let _permit = crate::runtime::grounding::config::claim_search_permits()
                    .acquire()
                    .await;
                let idx = match engine.open(&path).await {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::warn!(
                            target: "grounding_gate",
                            corpus = %corpus_id,
                            error = %e,
                            "claim search: open_index failed"
                        );
                        return None;
                    }
                };
                let t_corpus = std::time::Instant::now();
                let hit = idx.search(&embedding, &claim, CLAIM_SEARCH_K).await;
                let corpus_ms = t_corpus.elapsed().as_millis() as u64;
                match hit {
                    Ok(hits) if !hits.is_empty() => Some((corpus_id, hits, corpus_ms)),
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!(
                            target: "grounding_gate",
                            corpus = %corpus_id,
                            error = %e,
                            "claim search: search failed"
                        );
                        None
                    }
                }
            });
        }
        let per_corpus: Vec<(String, Vec<String>, u64)> = futures::stream::iter(tasks)
            .buffered(crate::runtime::grounding::config::claim_search_concurrency())
            .filter_map(|r| async move { r })
            .collect()
            .await;
        // Round-robin across corpora up to the cap.
        let mut out: Vec<String> = Vec::new();
        // Chunks each corpus actually CONTRIBUTED to `out`, index-aligned with
        // `per_corpus`. The fan-out fetches `CLAIM_SEARCH_K` per corpus and
        // keeps `CLAIM_SEARCH_K` in total, so everything past the cap is paid
        // for and discarded — this is what makes that visible (ARCH §0.1).
        let mut yielded = vec![0usize; per_corpus.len()];
        let mut rank = 0usize;
        while out.len() < CLAIM_SEARCH_K {
            let mut any = false;
            for (i, (_, corpus_hits, _)) in per_corpus.iter().enumerate() {
                if let Some(c) = corpus_hits.get(rank) {
                    any = true;
                    if out.len() < CLAIM_SEARCH_K {
                        out.push(c.clone());
                        yielded[i] += 1;
                    }
                }
            }
            if !any {
                break;
            }
            rank += 1;
        }
        let fetched: usize = per_corpus.iter().map(|(_, h, _)| h.len()).sum();
        let per_corpus_cost: Vec<(String, usize, usize, u64)> = per_corpus
            .iter()
            .enumerate()
            .map(|(i, (id, hits, ms))| (id.clone(), hits.len(), yielded[i], *ms))
            .collect();
        tracing::info!(
            target: "grounding_gate",
            event = "claim_search",
            claim = %claim.chars().take(90).collect::<String>(),
            corpora = per_corpus.len(),
            fetched,
            used = out.len(),
            discarded = fetched.saturating_sub(out.len()),
            elapsed_ms = t_claim.elapsed().as_millis() as u64,
            per_corpus = ?per_corpus_cost,
            "claim search: per-corpus fan-out cost and yield (corpus, fetched, yielded, ms)"
        );
        out
    }
}

#[cfg(test)]
mod tests {
    //! `search_corpus`'s deciders, against a fake index source. Zero tests
    //! existed for them before 2026-09-02 because the searcher bound a concrete
    //! `CorpusEngine`; one of them (permit before open) is the fix for a
    //! memory event.
    use super::*;
    use crate::runtime::grounding::config;
    use crate::types::{CompletionRequest, CompletionResponse, ProviderCapabilities};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    struct EmbedOnly;
    #[async_trait::async_trait]
    impl InferenceProvider for EmbedOnly {
        async fn complete(&self, _: &CompletionRequest) -> crate::Result<CompletionResponse> {
            Err(crate::Error::NotImplemented("EmbedOnly".into()))
        }
        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> crate::Result<Pin<Box<dyn Stream<Item = crate::Result<String>> + Send>>> {
            Err(crate::Error::NotImplemented("EmbedOnly".into()))
        }
        async fn embed(&self, _: &str) -> crate::Result<Vec<f32>> {
            Ok(vec![0.1, 0.2])
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: crate::types::Depth::Moderate,
            }
        }
    }

    /// Every corpus opens after `open_ms` and answers `hits_per_corpus`
    /// distinct hits. Counts opens and the PEAK number of concurrently open
    /// (open-or-searching) indexes — the only observable that tells "permit
    /// held across open and search" from "permit taken after open".
    struct FakeSource {
        corpora: Vec<SealedIndexRef>,
        open_ms: u64,
        hits_per_corpus: usize,
        opens: AtomicUsize,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl FakeSource {
        fn knowledge(n: usize, open_ms: u64, hits_per_corpus: usize) -> Arc<Self> {
            Arc::new(Self {
                corpora: (0..n)
                    .map(|i| SealedIndexRef {
                        corpus_id: format!("c{i}"),
                        kind: corpus_engine::CorpusKind::Knowledge,
                        path: PathBuf::from(format!("/fake/c{i}")),
                    })
                    .collect(),
                open_ms,
                hits_per_corpus,
                opens: AtomicUsize::new(0),
                in_flight: Default::default(),
                peak: Default::default(),
            })
        }
    }

    struct FakeIndex {
        corpus_id: String,
        hits: usize,
        in_flight: Arc<AtomicUsize>,
    }

    impl Drop for FakeIndex {
        fn drop(&mut self) {
            self.in_flight.fetch_sub(1, SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl SealedIndexSource for FakeSource {
        async fn usable(&self) -> corpus_engine::Result<Vec<SealedIndexRef>> {
            Ok(self.corpora.clone())
        }
        async fn open(&self, path: &Path) -> corpus_engine::Result<Arc<dyn SealedIndex>> {
            self.opens.fetch_add(1, SeqCst);
            let cur = self.in_flight.fetch_add(1, SeqCst) + 1;
            self.peak.fetch_max(cur, SeqCst);
            if self.open_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.open_ms)).await;
            }
            let corpus_id = path.file_name().unwrap().to_string_lossy().to_string();
            Ok(Arc::new(FakeIndex {
                corpus_id,
                hits: self.hits_per_corpus,
                in_flight: Arc::clone(&self.in_flight),
            }))
        }
    }

    #[async_trait::async_trait]
    impl SealedIndex for FakeIndex {
        async fn search(&self, _: &[f32], _: &str, k: usize) -> corpus_engine::Result<Vec<String>> {
            Ok((0..self.hits.min(k))
                .map(|r| format!("{}#{r}", self.corpus_id))
                .collect())
        }
    }

    fn searcher(source: Arc<FakeSource>, allowed: &[&str]) -> ClaimSearcher {
        ClaimSearcher::over(
            Arc::new(EmbedOnly),
            source,
            allowed.iter().map(|s| s.to_string()).collect(),
        )
    }

    /// THE PERMIT COVERS THE OPEN. Two claims searched at once over more
    /// corpora than the bound: without the permit, or with it taken after
    /// `open`, the two inner fan-outs each open `C` at a time and the peak is
    /// `2C`. With it, the peak is `C` — the one bound the memory event needed.
    /// FAILS IF: the permit is removed, taken after `open`, or made per-call.
    #[tokio::test]
    async fn the_permit_bounds_open_and_search_across_nested_fanouts() {
        let c = config::claim_search_concurrency();
        let n = 2 * c + 2;
        let src = FakeSource::knowledge(n, 30, 1);
        let allowed: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
        let allowed_ref: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        let s1 = searcher(Arc::clone(&src), &allowed_ref);
        let s2 = searcher(Arc::clone(&src), &allowed_ref);
        let (a, b) = tokio::join!(s1.search_corpus("claim one"), s2.search_corpus("claim two"));
        assert_eq!(a.len(), CLAIM_SEARCH_K);
        assert_eq!(b.len(), CLAIM_SEARCH_K);
        assert_eq!(
            src.opens.load(SeqCst),
            2 * n,
            "every allowed corpus opened, per claim"
        );
        let peak = src.peak.load(SeqCst);
        assert!(
            peak <= c,
            "peak {peak} concurrent open-or-searching indexes exceeds the bound {c}"
        );
        if c > 1 {
            assert!(peak > 1, "the fan-out must actually overlap (peak {peak})");
        }
    }

    /// The allow-list is a HARD seal: an index the source lists is never
    /// opened unless its corpus id is allowed, exactly.
    #[tokio::test]
    async fn only_allowed_corpora_are_opened() {
        let src = FakeSource::knowledge(4, 0, 1);
        let out = searcher(Arc::clone(&src), &["c1", "c3"])
            .search_corpus("x")
            .await;
        assert_eq!(src.opens.load(SeqCst), 2);
        assert_eq!(out, vec!["c1#0".to_string(), "c3#0".to_string()]);
        assert!(searcher(Arc::clone(&src), &[])
            .search_corpus("x")
            .await
            .is_empty());
        assert_eq!(src.opens.load(SeqCst), 2, "an empty seal opens nothing");
    }

    /// Only Knowledge and Catalog indexes are searched; a code index in the
    /// same store is skipped even when allowed.
    #[tokio::test]
    async fn non_knowledge_indexes_are_never_opened() {
        let mut src = FakeSource::knowledge(2, 0, 1);
        Arc::get_mut(&mut src).unwrap().corpora[1].kind = corpus_engine::CorpusKind::Code;
        let out = searcher(Arc::clone(&src), &["c0", "c1"])
            .search_corpus("x")
            .await;
        assert_eq!(src.opens.load(SeqCst), 1);
        assert_eq!(out, vec!["c0#0".to_string()]);
    }

    /// The round-robin keeps `CLAIM_SEARCH_K` in total, interleaved by rank
    /// across corpora in the source's order, so the output is deterministic
    /// whatever the completion order was.
    #[tokio::test]
    async fn the_round_robin_interleaves_by_rank_and_caps_at_k() {
        let src = FakeSource::knowledge(3, 5, 4);
        let out = searcher(src, &["c0", "c1", "c2"]).search_corpus("x").await;
        assert_eq!(
            out,
            vec!["c0#0", "c1#0", "c2#0", "c0#1"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    /// A source with no usable index degrades to "no hits", never to an
    /// error the audit would have to handle.
    #[tokio::test]
    async fn an_empty_store_yields_nothing() {
        let src = FakeSource::knowledge(0, 0, 0);
        assert!(searcher(src, &["c0"]).search_corpus("x").await.is_empty());
    }
}
