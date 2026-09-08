// SPDX-License-Identifier: AGPL-3.0-or-later
//! Atlas grounding: ANN-navigate graph-walk chunk injection
//! with bag-of-atoms fallback, plus the direct chunk-by-id
//! fetch it (and atom-enum) uses.

use std::sync::Arc;

use super::super::*;

impl Runtime {
    /// Search all installed corpus-engine LanceDB indexes.
    ///
    /// Returns scored chunks from every installed corpus. If the IVF-PQ
    /// vector index is not built for a corpus, passes an empty embedding
    /// to trigger FTS-only mode (fast Tantivy, avoids the 20–60 second
    /// O(n) full-scan fallback).
    ///
    /// Used by both `handle_knowledge_query` and `handle_simple` so that
    /// installed corpora enrich all intent types, not just KnowledgeQuery.
    /// Apply atlas grounding to a chunk pool: graph-walk navigation
    /// when the provider exposes the graph layer, falling back to
    /// bag-of-atoms top-K otherwise. Idempotent — appends to `chunks`
    /// in place; no-op when atlas grounding is disabled, no provider
    /// is registered, or the embedding is empty.
    ///
    /// `label` is the call-site identifier surfaced to logs and
    /// downstream search-corpus-indexes traces (e.g. "KnowledgeQuery"
    /// vs "DeepQuery") so operators can track which retrieval path
    /// generated which atlas additions.
    ///
    /// Single canonical implementation; both intent paths
    /// (KnowledgeQuery + DeepQuery) call this rather than inlining
    /// the ~80-line graph-walk + fallback block.
    /// Fetch a single chunk by its LanceDB row id from a specific
    /// corpus. Used by atlas-grounding's direct-fetch path for atom
    /// shapes whose `first_appearance.chunk_id` is numeric
    /// (conversation, personal-vault) — bypassing the SEP/Wikipedia
    /// FTS-by-article-slug path that doesn't apply when chunks
    /// aren't titled by article. Returns `None` on any failure
    /// (corpus not installed, index open failure, chunk_id not
    /// present) — caller treats absence as a no-op.
    ///
    /// Opens the index per call. Acceptable today: the atlas-fetch
    /// loop budget is small (~6 requests / query); opening is
    /// dominated by the LanceDB manifest read which is cached after
    /// the first hit. If profiling shows this is hot, the right
    /// optimisation is a per-call index cache in `apply_atlas_grounding`,
    /// not memoising across queries (atlas-grounding fires once per
    /// chat turn).
    pub(crate) async fn fetch_chunk_by_id(
        &self,
        corpus_id: &str,
        chunk_id: u64,
    ) -> Option<corpus_engine::ScoredChunk> {
        let engine = self.corpus_engine.as_ref()?;
        let indexes = engine.usable_indexes().await.ok()?;
        let info = indexes.into_iter().find(|i| i.corpus_id == corpus_id)?;
        let index = corpus_engine::index::CorpusIndex::open(&info.path)
            .await
            .ok()?;
        // Through the index's own re-acquisition door rather than rebuilt
        // here: this IS index content, and assembling it by hand is how real
        // corpus passages entered the pool with no provenance (TOPOLOGY §10
        // rung 9.1, hazard 1).
        let acquired = index.acquire_chunks(&[chunk_id]).await.ok()?;
        acquired.into_iter().next()
    }
    /// `summaries_out` carries ei-7a's rollups OUT of the walk instead of
    /// appending them here. That is the whole point of the parameter: this
    /// function runs at pipeline rung 8, BEFORE reweight/rerank, and a summary
    /// entering there is the −14pt displacement the late-injection design was
    /// built to avoid (`project_raptor_retrieval_grounding`). The walk reaches
    /// the Summary atoms; the LATE site appends them. Both facts have to be
    /// true at once, which is why they are two places.
    ///
    /// It is APPENDED to, never cleared, so a caller that grounds twice in a
    /// turn accumulates rather than losing the first walk's summaries.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn apply_atlas_grounding(
        &self,
        query_text: &str,
        embedding: &[f32],
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        summaries_out: &mut Vec<corpus_engine::enrichment::atlas::ground::SummaryNode>,
        label: &str,
        scope: Option<&str>,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
        lane: &crate::runtime::Lane,
    ) -> crate::runtime::retrieval_ledger::StepLedger {
        use crate::runtime::retrieval_ledger::{DropReason, StepLedger};
        use corpus_engine::enrichment::atlas::ground;
        // THE THREE WAYS THIS STEP DOES NOTHING, each said out loud.
        //
        // All three used to return an empty ledger in silence, and from outside
        // they are indistinguishable from "the atlas had nothing to add" — the
        // §18.3 shape, an absence defaulted. It cost an afternoon on
        // 2026-09-04: wikipedia contributed nothing to a prod-pipeline eval,
        // the store class was blamed, and the store was never asked because
        // this step returned before it got there.
        if !atlas_grounding_enabled() {
            tracing::info!(
                label,
                reason = "feature-disabled",
                "atlas-grounding: skipped (SOVEREIGN_ATLAS_GROUNDING is off)"
            );
            return StepLedger::injected(0).drop(DropReason::FeatureDisabled, 0);
        }
        let Some(provider) = lane.atlas_context.as_ref() else {
            tracing::info!(
                label,
                reason = "no-atlas-provider-on-lane",
                "atlas-grounding: skipped — this lane carries no atlas provider, so NO store \
                 of any class can be reached from it"
            );
            return StepLedger::injected(0);
        };
        if embedding.is_empty() {
            tracing::info!(
                label,
                reason = "empty-query-embedding",
                "atlas-grounding: skipped (no query embedding)"
            );
            return StepLedger::injected(0);
        }

        // Scope atlas grounding to the corpora retrieval actually hit — not
        // every loaded atlas (at SEP's 1778-atlas scale that meant a
        // brute-force ANN seed over all of them, every query). Per retrieved
        // chunk the candidate atlases are `candidate_atlas_ids`' answer, which
        // is the ONE home of the chunk -> atlas id derivation; the
        // `format!("{}-{}", corpus_id, title)` that used to sit here encoded
        // SEP's per-article layout as a universal rule at a call site that
        // could not be tested. `ensure_loaded` lazily warms only these;
        // `provider.get(id)` below drops any with no atlas. `enabled_corpora`
        // (conversation scope) is folded in so an explicitly scoped corpus
        // grounds even if its chunks didn't rank this turn.
        let mut scoped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in chunks.iter() {
            scoped.extend(ground::candidate_atlas_ids(
                &c.corpus_id,
                c.title.as_deref(),
            ));
        }
        if let Some(enabled) = enabled_corpora {
            scoped.extend(enabled.iter().cloned());
        }
        let mut corpus_ids: Vec<String> = scoped.into_iter().collect();
        // WHICH CORPORA WERE EVEN CONSIDERED. Without this, an empty set and a
        // set whose stores all refused look identical from outside — both
        // simply produce no grounding and no line. That ambiguity cost real
        // time on 2026-09-04: wikipedia contributed nothing to a prod-pipeline
        // eval and the store class was blamed, when the candidate set was
        // empty and no store was ever asked. Absence reported, not inferred
        // (ARCH §18.3).
        tracing::info!(
            label,
            candidates = corpus_ids.len(),
            corpora = %corpus_ids.join(","),
            from_chunks = chunks.len(),
            scoped_by_conversation = enabled_corpora.map(|e| e.len()).unwrap_or(0),
            "atlas-grounding: candidate atlas corpora for this query"
        );
        provider.ensure_loaded(&corpus_ids).await;
        // Scope-driven atlas filtering. When the router classifies
        // the query against a `scope = "personal"`-tagged exemplar
        // (conversation-history / personal-vault shapes), restrict
        // the atlas pool to user-owned corpora (mesh_sharing=false
        // in IndexInfo). Without this, large public atlases
        // (wikipedia at 1.6M atoms) drown small personal atlases
        // (conversations-personal at ~200) in the global cosine
        // race. The router's nearest exemplar is the load-bearing
        // signal; downstream retrieval honors it here.
        if scope == Some("personal") {
            // Same sharp-signal limitation as the lance-side filter
            // in `prepare_knowledge_query_plan` — see that block for
            // the rationale + TODO. Pattern match is the immediate
            // demonstrable wiring; recipe annotation is the proper
            // long-form productionization.
            const PERSONAL_CORPUS_PREFIXES: &[&str] =
                &["conversations-", "personal-", "journal-", "inner-work-"];
            let before = corpus_ids.len();
            corpus_ids.retain(|id| PERSONAL_CORPUS_PREFIXES.iter().any(|p| id.starts_with(p)));
            if before != corpus_ids.len() {
                tracing::info!(
                    label,
                    kept = corpus_ids.len(),
                    dropped = before - corpus_ids.len(),
                    scope = "personal",
                    "atlas-grounding: scope-filtered to personal-corpus prefixes"
                );
            }
        }
        let ctxs: Vec<Arc<crate::atlas_context::AtlasContext>> = corpus_ids
            .iter()
            .filter_map(|id| provider.get(id))
            .collect();
        // `walk_provider`, not `graph`: the walk reads an `AtlasProvider`, and
        // for a wiki-class corpus there is no `AtlasGraph` to hand back. Asking
        // for the concrete type here is what kept wikipedia on the
        // bag-of-atoms branch below no matter what store it had.
        let graphs: Vec<Arc<dyn corpus_engine::enrichment::atlas::AtlasProvider>> = corpus_ids
            .iter()
            .filter_map(|id| {
                let p = provider.walk_provider(id);
                // WHICH STORE ANSWERED is a decision, so it is visible (ARCH §9,
                // principle 1). It lands here rather than at the open site
                // because this crate is in the eval subscriber's default filter
                // (`sovereign_core=info`) while the manager's is not — the
                // allowlist trap: a `debug!` on an unlisted target is dark, and
                // chasing this exact question through dark targets is what cost
                // an afternoon. Once per corpus per grounding call, not once per
                // load: the open behind it IS memoised, and repeating the line
                // per question is what lets a lane row assert that every
                // question in an arm was served by the same class.
                match &p {
                    Some(g) => tracing::info!(
                        label,
                        corpus = id.as_str(),
                        provider_class = g.provider_class(),
                        seed_table = g.has_ann_seed_table(),
                        "atlas-grounding: store class serving this corpus"
                    ),
                    None => tracing::info!(
                        label,
                        corpus = id.as_str(),
                        provider_class = "none",
                        "atlas-grounding: no store the walk can read — bag-of-atoms for this corpus"
                    ),
                }
                p
            })
            .collect();

        if graphs.is_empty() {
            // No graph layer loaded for any provider. Direct bag-of-
            // atoms injection — kept for older deployments + as a
            // safety net during graph-layer rollout.
            let mut bag_added = 0usize;
            for corpus_id in &corpus_ids {
                if let Some(ctx) = provider.get(corpus_id) {
                    let virt = crate::atlas_context::atlas_top_k_as_chunks(embedding, &ctx);
                    for chunk in &virt {
                        if let Some(name) = chunk.title.as_deref() {
                            provider.record_match(corpus_id, name);
                        }
                    }
                    bag_added += virt.len();
                    chunks.extend(virt);
                }
            }
            if bag_added > 0 {
                tracing::info!(
                    label,
                    bag_added,
                    "atlas-grounding: bag-of-atoms fused (graph layer absent)"
                );
            }
            // The bag path realises every candidate it builds, so
            // considered == added and the identity holds with no drops.
            return StepLedger::injected(bag_added);
        }

        // ── The walk, in corpus-engine, driven by the corpus's own map ──
        //
        // Everything from here to the fetch is `ground`'s: the seeds, the
        // hops, the edge kinds and the budget come from the atlas's
        // navigation policy (`EPISTEMIC_INDEX.md` §2.2) instead of from four
        // constants that used to live at this call site. What remains here is
        // what only a Runtime can do — pick the atlases, embed through the
        // lane's provider, and fetch a chunk.
        let ctx_refs: Vec<&crate::atlas_context::AtlasContext> =
            ctxs.iter().map(|c| c.as_ref()).collect();
        // Already the trait — `walk_provider` widened at the source, so the
        // walk below neither knows nor needs to know which store is behind
        // them.
        let graph_refs: Vec<&dyn corpus_engine::enrichment::atlas::AtlasProvider> =
            graphs.iter().map(|g| g.as_ref()).collect();
        let max_seeds = ctxs.first().map(|c| c.top_k).unwrap_or(3).max(12);
        let (policy, policy_source) = ground::navigation_policy_for(&graph_refs);
        // The QUERY-side adapter: the classifier's centroids must sit in the
        // same vector space as the question, which is the space the ANN seed
        // tables were built in (see `embed_fn.rs`). Built per call and cheap —
        // `shared_classifier` embeds the exemplars once per process.
        let embed = crate::embed_fn::inference_to_embed_query_fn(Arc::clone(&self.inference));
        let inventory = corpus_engine::enrichment::atlas::AtlasInventory::of(&graph_refs);
        let selection =
            ground::select_walk(embedding, &policy, policy_source, &inventory, Some(&embed)).await;
        tracing::debug!(
            target: "retrieval_audit",
            label,
            corpora = graph_refs.len(),
            max_seeds,
            row = %selection.describe(),
            "atlas-grounding: walking the map"
        );

        let grounding = ground::ground(
            query_text,
            embedding,
            &ctx_refs,
            &graph_refs,
            &selection,
            max_seeds,
        )
        .await;
        for d in &grounding.degradations {
            // NAMED, never defaulted (ARCH §18.3). A degraded walk and a walk
            // over a thin atlas produce the same small number; only this line
            // tells them apart.
            tracing::info!(
                label,
                kind = grounding.kind.as_str(),
                "atlas-grounding: {}",
                d.sentence()
            );
        }

        // ei-7a: hand the walk's rollups to the caller. Counted here rather
        // than only at the append site, because "the walk reached none" and
        // "the late site dropped them" are different facts and only two
        // numbers can tell them apart (ARCH §18.3).
        if !grounding.summaries.is_empty() {
            tracing::info!(
                label,
                summaries = grounding.summaries.len(),
                "atlas-grounding: Summary atoms carried out for late append"
            );
            summaries_out.extend(grounding.summaries.iter().cloned());
        }
        // The "this corpus has RAPTOR rows and no Summary atoms" line that
        // stood here is GONE, and not because the question stopped mattering.
        // RAPTOR is a composed SOURCE now (operator directive 25ae5815), so
        // the honest report is not "one thing is missing" but "here is what
        // each source served" — the walk's own ledger carries
        // `summary_sources=[atoms:N,raptor:M]` on the `retrieval_audit`
        // target, which says absence and presence with one mechanism and does
        // it for every source at once. It also fixes where the old line lived:
        // an untargeted `info!` is dark under the `warn,retrieval_audit=debug`
        // filter every lane sets, so the line I wrote to make an absence
        // visible was itself unobservable (ARCH §9's allowlist trap, caught by
        // ei-5c's own lane reporting "none logged" when it could not know).

        let before = chunks.len();
        let fetcher = RuntimeEvidenceFetcher {
            runtime: self,
            corpus_ceiling,
            lane,
        };
        let (fetched, resolve) = ground::resolve_evidence(
            &grounding.requests,
            grounding.budget,
            enabled_corpora,
            &fetcher,
        )
        .await;
        let mut seen_in_pool: std::collections::HashSet<String> = std::collections::HashSet::new();
        for r in fetched {
            let key = format!(
                "{}|{}",
                r.chunk.title.clone().unwrap_or_default(),
                truncate_chars(&r.chunk.content, 80)
            );
            if seen_in_pool.insert(key) {
                chunks.push(r.chunk);
            }
        }
        let graph_added = chunks.len() - before;

        // The line whose absence hid the defect: candidates in, chunks out,
        // and every drop accounted for by reason.
        tracing::info!(
            label,
            considered = resolve.considered,
            graph_added,
            dropped_not_allowed = resolve.out_of_scope,
            dropped_not_found = resolve.unresolvable,
            dropped_no_title_match = resolve.title_mismatch,
            dropped_duplicate = resolve.duplicate,
            "atlas-grounding: fetch ledger"
        );
        let ledger = StepLedger::injected(resolve.considered)
            .drop(DropReason::OutOfScope, resolve.out_of_scope)
            .drop(DropReason::EvidenceUnresolvable, resolve.unresolvable)
            .drop(DropReason::TitleMismatch, resolve.title_mismatch)
            .drop(DropReason::Duplicate, resolve.duplicate)
            // Candidates past the fetch budget were never attempted. They
            // are a DECISION, not a failure, and the accounting identity
            // requires them named.
            .drop(DropReason::BudgetExhausted, resolve.budget_exhausted());

        // Adaptive triage: bump article slug per atlas to climb
        // the Tier-2 enrichment queue.
        for ctx in &ctxs {
            provider.record_match(&ctx.atlas_corpus_id, &ctx.atlas_corpus_id);
        }
        if graph_added > 0 {
            // Per-corpus breakdown of what graph-walk just pushed,
            // so a downstream drop (cap / truncate / expand) can
            // be pinned by comparing this against later sites
            // (ARCH §0.1 glassbox).
            let mut per_corpus: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            let n = chunks.len();
            for c in chunks.iter().skip(n - graph_added.min(n)) {
                *per_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
            }
            tracing::info!(
                label,
                graph_added,
                per_corpus = ?per_corpus,
                "atlas-grounding: graph-walk fused (per-corpus injected counts)"
            );
        }
        ledger
    }
}

/// The two fetches the walk's resolve step needs, over a live [`Runtime`].
///
/// Nothing but I/O: scope, budget, title filter, duplicate identity and
/// scoring are all decided inside `corpus_engine`'s `resolve_evidence`, which
/// `corpus-mcp` reaches through the same door. This type is the reason the
/// two hosts cannot drift into two different walks (§10.6).
struct RuntimeEvidenceFetcher<'a> {
    runtime: &'a Runtime,
    corpus_ceiling: Option<&'a [String]>,
    lane: &'a crate::runtime::Lane,
}

impl corpus_engine::enrichment::atlas::ground::EvidenceFetcher for RuntimeEvidenceFetcher<'_> {
    async fn by_row(
        &self,
        corpus: &kernel_types::CorpusId,
        row: u64,
    ) -> Option<corpus_engine::ScoredChunk> {
        self.runtime.fetch_chunk_by_id(corpus.as_str(), row).await
    }

    async fn by_search(
        &self,
        corpus: &kernel_types::CorpusId,
        query: &str,
        limit: usize,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let scope = [corpus.as_str().to_string()];
        self.runtime
            .search_corpus_indexes_with_overrides(
                &[],
                query,
                limit,
                "AtlasNavigate",
                None,
                Some(&scope),
                self.corpus_ceiling,
                self.lane,
            )
            .await
    }
}

/// One walked [`SummaryNode`] as a virtual chunk, for the LATE append site.
///
/// The sibling of `raptor_grounding::raptor_scored_chunk` and deliberately
/// NOT in that file: `raptor_grounding.rs` is the injector this port retires,
/// and a constructor the walk depends on must not be deleted with it.
///
/// The differences from the injector's chunk are the two that matter:
///
/// - `ChunkProvenance::manufactured_summary("atlas_summary")` — same typed
///   grain (`Grain::Summary`, so `reserve_summary_chunks` and every
///   quotability gate treat it identically), different PRODUCER, so a trace
///   or a ledger can say which of the two paths put a rollup in the pool.
/// - `atom_id` in the metadata rather than `raptor_node_id`: the atlas atom is
///   what this came from, and it is the handle that resolves back to the
///   atom's own evidence chunks.
///
/// `metadata["source"] = "raptor"` is kept for one reason: the FORMATTERS and
/// the bench's own accounting still read that tag to label a summary in the
/// prompt, and a rollup that the prompt cannot label is worse than one whose
/// tag is a legacy name. The RESERVE decision no longer reads it (§10.6).
pub(crate) fn atlas_summary_chunk(
    node: &corpus_engine::enrichment::atlas::ground::SummaryNode,
) -> corpus_engine::ScoredChunk {
    let corpus_id = node.site.chunk_corpus().as_str().to_string();
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("source".to_string(), "raptor".to_string());
    metadata.insert("summary_producer".to_string(), "atlas_summary".to_string());
    if !node.atom_id.is_empty() {
        metadata.insert("atom_id".to_string(), node.atom_id.clone());
    }
    corpus_engine::ScoredChunk {
        content: node.text.clone(),
        // The article when the site has one (`sep-abduction` -> `abduction`),
        // else the corpus's own name: the title is what the prompt labels the
        // passage with, and a summary with no label reads as an orphan.
        title: Some(
            node.site
                .article()
                .map(str::to_string)
                .unwrap_or_else(|| node.site.chunk_corpus().as_str().to_string()),
        ),
        url: None,
        corpus_id,
        score: node.score,
        metadata,
        chunk_id: None,
        source_doc_id: None,
        vector_distance: Some(1.0 - node.score),
        provenance: corpus_engine::index::ChunkProvenance::manufactured_summary("atlas_summary"),
    }
}

/// Append `summaries` to `chunks` at the LATE position, then reserve every
/// summary-grain chunk to the head — the two halves of the placement the
/// injector's own history proved are both required (injection TIMING buys
/// leaf-ranking neutrality; head PLACEMENT is what keeps the char budget from
/// cutting them straight back out). Returns how many were appended.
///
/// Deliberately does the reserve itself rather than leaving it to the caller:
/// the one time it was left to a caller, the summaries were appended at the
/// tail and admitted at zero for four months.
pub(crate) fn append_atlas_summaries(
    chunks: &mut Vec<corpus_engine::ScoredChunk>,
    summaries: &[corpus_engine::enrichment::atlas::ground::SummaryNode],
    label: &str,
) -> usize {
    if summaries.is_empty() {
        return 0;
    }
    for node in summaries {
        chunks.push(atlas_summary_chunk(node));
    }
    let appended = summaries.len();
    let taken = std::mem::take(chunks);
    *chunks = crate::runtime::question_analysis::reserve_summary_chunks(taken);
    tracing::info!(
        label,
        appended,
        pool = chunks.len(),
        "atlas-grounding: Summary atoms appended late and reserved"
    );
    appended
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The map's `DEFAULT_BUDGET` and this file's live fetch budget are ONE
    /// number and must move together.
    ///
    /// They were not: `EPISTEMIC_INDEX.md` §1's Walk row said "budget 6", the
    /// pre-registered default was minted from that sentence, and the live
    /// call site had computed `ceil(20 * 0.6) = 12` since the SEP
    /// calibration. Adopting the map's number would therefore have HALVED
    /// atlas evidence on every corpus at the moment the walk started reading
    /// the map — a regression handed to the SEP lane by a doc typo. The
    /// default was corrected to the code (principle 4), and this test is what
    /// keeps the correction from rotting: it fails if either side moves
    /// alone.
    ///
    /// Failing input: set `DEFAULT_BUDGET` back to 6, or change
    /// `KQ_PER_CORPUS_LIMIT` without re-deriving it.
    #[test]
    fn the_default_budget_is_the_live_fetch_budget() {
        let live = ((KQ_PER_CORPUS_LIMIT as f32) * 0.6).ceil() as u32;
        assert_eq!(
            live,
            corpus_engine_vocab::ontology::DEFAULT_BUDGET,
            "the navigation table's default budget ({}) and the retrieval \
             fetch budget ({live}) are one number",
            corpus_engine_vocab::ontology::DEFAULT_BUDGET
        );
    }

    /// The chunk -> atlas id derivation this file used to inline is the one in
    /// corpus-engine, and it still produces what the SEP scope needs: the
    /// parent corpus and its per-article child.
    ///
    /// Failing input: reinstate `format!("{}-{}", corpus_id, title)` here.
    #[test]
    fn the_scope_derivation_is_corpus_engines_and_still_reaches_sep_articles() {
        use corpus_engine::enrichment::atlas::ground::candidate_atlas_ids;
        let ids = candidate_atlas_ids("sep", Some("freewill"));
        assert!(ids.contains(&"sep".to_string()));
        assert!(ids.contains(&"sep-freewill".to_string()));
        // A titleless chunk scopes to its own corpus only — never to a
        // "<corpus>-" id that addresses nothing.
        assert_eq!(
            candidate_atlas_ids("wikipedia", None),
            vec!["wikipedia".to_string()]
        );
    }
}

#[cfg(test)]
mod atlas_summary_append_tests {
    use super::{append_atlas_summaries, atlas_summary_chunk};
    use corpus_engine::enrichment::atlas::evidence_site::EvidenceSite;
    use corpus_engine::enrichment::atlas::ground::SummaryNode;
    use corpus_engine::index::ChunkProvenance;
    use corpus_engine::ScoredChunk;

    fn leaf(i: usize) -> ScoredChunk {
        ScoredChunk {
            content: format!("leaf {i}"),
            title: Some("abduction".into()),
            url: None,
            corpus_id: "sep".into(),
            score: 0.9,
            metadata: Default::default(),
            chunk_id: Some(i as u64),
            source_doc_id: None,
            vector_distance: Some(0.1),
            // A real acquired LEAF: `Grain::Leaf`, so the reserve's grain
            // predicate must NOT pick it up. Using a manufactured summary here
            // would make every assertion below vacuous.
            provenance: ChunkProvenance::acquired_from_estate("sep"),
        }
    }

    fn node(score: f32) -> SummaryNode {
        SummaryNode {
            atom_id: "summary-abcdef0123456789".into(),
            site: EvidenceSite::derive("sep-abduction"),
            text: "a rollup".into(),
            score,
        }
    }

    /// The failure this whole placement exists to prevent, in the direction it
    /// actually happened: a summary appended at the TAIL of a full pool is cut
    /// by the char budget and the pool truncate before the prompt ever sees it
    /// (measured on `summary_proof_theory`: pool=40, admitted=28,
    /// raptor_admitted=0 of 8; invariant 3035f3a4). So the append must leave
    /// the summary at the HEAD.
    #[test]
    fn a_late_appended_summary_lands_at_the_head_not_the_tail() {
        let mut pool: Vec<ScoredChunk> = (0..5).map(leaf).collect();
        let appended = append_atlas_summaries(&mut pool, &[node(0.5)], "test");
        assert_eq!(appended, 1);
        assert_eq!(pool.len(), 6, "the chunk SET must be unchanged in size");
        assert_eq!(
            pool[0].provenance.producer(),
            Some("atlas_summary"),
            "the summary must be first, or the budget cuts it"
        );
        // Order-only: every leaf is still present, in its original order.
        let leaves: Vec<&str> = pool[1..].iter().map(|c| c.content.as_str()).collect();
        assert_eq!(
            leaves,
            vec!["leaf 0", "leaf 1", "leaf 2", "leaf 3", "leaf 4"]
        );
    }

    /// The other direction (§18.6): with no summaries the pool is returned
    /// untouched — no reserve, no reorder, no allocation of a different order.
    /// A "fix" that reordered every pool would be invisible in the test above
    /// and would perturb every lane that has no summaries at all.
    #[test]
    fn a_walk_that_reached_no_summary_leaves_the_pool_alone() {
        let mut pool: Vec<ScoredChunk> = (0..5).map(leaf).collect();
        let before: Vec<String> = pool.iter().map(|c| c.content.clone()).collect();
        let appended = append_atlas_summaries(&mut pool, &[], "test");
        assert_eq!(appended, 0);
        let after: Vec<String> = pool.iter().map(|c| c.content.clone()).collect();
        assert_eq!(before, after);
    }

    /// The chunk carries the SUMMARY grain (so every quotability gate and the
    /// reserve treat it as one) and its OWN producer (so a trace can say which
    /// of the two paths put it in the pool). Both halves matter: one grain,
    /// two producers.
    #[test]
    fn the_summary_chunk_is_summary_grain_with_its_own_producer() {
        let c = atlas_summary_chunk(&node(0.7));
        assert_eq!(c.provenance.grain(), kernel_types::Grain::Summary);
        assert_eq!(c.provenance.producer(), Some("atlas_summary"));
        // The chunk is attributed to the corpus that HOLDS the chunks, not to
        // the per-article atlas id — the same corpus a fetched chunk of that
        // article would carry, so the per-corpus ledgers stay comparable.
        assert_eq!(c.corpus_id, "sep");
        assert_eq!(c.title.as_deref(), Some("abduction"));
    }
}
