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
        let indexes = engine.installed_indexes().await.ok()?;
        let info = indexes.into_iter().find(|i| i.corpus_id == corpus_id)?;
        let index = corpus_engine::index::CorpusIndex::open(&info.path)
            .await
            .ok()?;
        let stored = index.get_chunks(&[chunk_id]).await.ok()?;
        let s = stored.into_iter().next()?;
        Some(corpus_engine::ScoredChunk {
            content: s.content,
            title: s.title,
            url: None,
            corpus_id: corpus_id.to_string(),
            score: 0.0,
            metadata: std::collections::HashMap::new(),
            chunk_id: Some(s.id),
            source_doc_id: None,
            vector_distance: None,
        })
    }
    pub(crate) async fn apply_atlas_grounding(
        &self,
        query_text: &str,
        embedding: &[f32],
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        label: &str,
        scope: Option<&str>,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) {
        if !atlas_grounding_enabled() {
            return;
        }
        let Some(provider) = self.atlas_context_provider.as_ref() else {
            return;
        };
        if embedding.is_empty() {
            return;
        }

        // Scope atlas grounding to the corpora retrieval actually hit — not
        // every loaded atlas (at SEP's 1778-atlas scale that meant a
        // brute-force ANN seed over all of them, every query). Per retrieved
        // chunk the candidate atlas is its own `corpus_id` plus
        // `<corpus_id>-<title>` for parent / per-article splits (SEP: the "sep"
        // chunk corpus -> "sep-<article>" atlases). `ensure_loaded` lazily
        // warms only these; `provider.get(id)` below drops any with no atlas.
        // `enabled_corpora` (conversation scope) is folded in so an explicitly
        // scoped corpus grounds even if its chunks didn't rank this turn.
        let mut scoped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for c in chunks.iter() {
            scoped.insert(c.corpus_id.clone());
            if let Some(t) = c.title.as_deref().filter(|t| !t.is_empty()) {
                scoped.insert(format!("{}-{}", c.corpus_id, t));
            }
        }
        if let Some(enabled) = enabled_corpora {
            scoped.extend(enabled.iter().cloned());
        }
        let mut corpus_ids: Vec<String> = scoped.into_iter().collect();
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
        let graphs: Vec<Arc<crate::atlas_context::AtlasGraph>> = corpus_ids
            .iter()
            .filter_map(|id| provider.graph(id))
            .collect();

        if !graphs.is_empty() {
            // Graph-walk: cosine seeds → BFS expand 1-2 hops over
            // typed edges (Tension / Grounds / Configures /
            // Involves) → aggregate evidence ChunkRefs across the
            // neighborhood → FTS-fetch each preview against the
            // source corpus filtered to the atom's article. Output
            // is real source chunks scored by atlas evidence
            // density. Validated +3 essay over baseline at 6-atlas
            // scale on the SEP eval.
            let ctx_refs: Vec<&crate::atlas_context::AtlasContext> =
                ctxs.iter().map(|c| c.as_ref()).collect();
            let graph_refs: Vec<&crate::atlas_context::AtlasGraph> =
                graphs.iter().map(|g| g.as_ref()).collect();
            let max_seeds = ctxs.first().map(|c| c.top_k).unwrap_or(3).max(12);
            // ATLAS_STORAGE_V2: one navigate. Each graph seeds from its persistent
            // ANN table (atom-ids directly, no per-query resolve) plus name-match
            // over the bag. The v1 sync cosine `atlas_navigate` was retired with
            // `resolve_atom_id_from_entry` — bags are derived from the ANN table,
            // so every loaded atlas already carries one.
            tracing::debug!(
                corpora = graph_refs.len(),
                max_seeds,
                "atlas-grounding: ANN navigate (v2)"
            );
            let requests = crate::atlas_context::atlas_navigate_ann(
                query_text,
                embedding,
                &ctx_refs,
                &graph_refs,
                max_seeds,
                /*max_hops=*/ 2,
            )
            .await;
            // Production budget mirrors the eval-CLI's calibrated
            // value (limit * 0.6, where limit is `KQ_PER_CORPUS_LIMIT
            // = 20`). Calibrated against the SEP bank: budget=6 gave
            // +22 sources / +6 essay / +6 dialectical_breadth vs
            // baseline; budget=4 left ~10 bank-required articles
            // unfetched even when their atlas was loaded.
            let fetch_budget = ((KQ_PER_CORPUS_LIMIT as f32) * 0.6).ceil() as usize;
            let mut graph_added = 0usize;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for req in requests.iter().take(fetch_budget * 2) {
                if graph_added >= fetch_budget {
                    break;
                }

                // Respect the conversation's corpus allow-list: an atom's
                // source corpus must itself be enabled before we fetch from
                // it. (Atlases load independently of the per-conversation
                // allow-list, so an atom can originate from a corpus the
                // conversation excluded.)
                if let Some(allowed) = enabled_corpora {
                    if !allowed.iter().any(|c| c == &req.corpus_id) {
                        continue;
                    }
                }

                // Shape-aware fetch. ChunkRequest.chunk_id is the
                // atom's first_appearance.chunk_id. For SEP/Wikipedia
                // atoms it's a section slug (`sec_00001`) and the
                // FTS-by-article-slug path below resolves it via
                // title-match. For conversation / personal-vault
                // atoms it's the numeric LanceDB row id — FTS+title-
                // match yields zero because the chunk title is the
                // conversation name, not the chunk_id. Detect
                // numeric ids and do a direct chunks_by_ids lookup
                // against the source corpus identified by
                // `article_slug` (which for non-SEP atlases is the
                // corpus_id itself, per AtlasGraph::load_from_disk
                // construction). Surfaced by conversations-personal
                // 2026-05-17: atlas atoms scored 0.7+ in
                // atlas_navigate but the FTS-fetch path produced
                // graph_added=0.
                if let Ok(chunk_id_num) = req.chunk_id.parse::<u64>() {
                    if let Some(mut boosted) = self
                        .fetch_chunk_by_id(&req.article_slug, chunk_id_num)
                        .await
                    {
                        let key = format!(
                            "{}|{}",
                            boosted.title.clone().unwrap_or_default(),
                            truncate_chars(&boosted.content, 80)
                        );
                        if seen.insert(key) {
                            boosted.score = req.score * 0.05;
                            // Make atlas-fetched chunks competitive
                            // in `cross_corpus_sort_cmp` against
                            // lance-fetched chunks (which carry
                            // vector_distance from hybrid search).
                            // Map atom relevance to a synthetic
                            // distance: high atlas score → low
                            // distance (sorted to top); the runtime's
                            // cross-corpus sort then keeps atlas
                            // chunks above lance fillers when
                            // truncating to KQ_MERGED_LIMIT.
                            boosted.vector_distance =
                                Some((1.0_f32 - (req.score / 2.0).min(1.0)).max(0.0));
                            if !req.verbatim_excerpts.is_empty() {
                                let mut head = String::from("[Atlas highlights]\n");
                                for ex in &req.verbatim_excerpts {
                                    head.push_str(ex);
                                    head.push('\n');
                                }
                                head.push('\n');
                                head.push_str(&boosted.content);
                                boosted.content = head;
                            }
                            chunks.push(boosted);
                            graph_added += 1;
                            if graph_added >= fetch_budget {
                                break;
                            }
                        }
                    }
                    continue;
                }

                // SEP/Wikipedia article-slug path. Scope the FTS fetch to
                // the atom's OWN corpus — the chunk lives there (the atlas
                // was extracted from it), so this selects the same chunk
                // the title filter would, without searching every other
                // enabled corpus per request (the 1.9M-chunk wikipedia
                // index was otherwise opened once per atom). `enabled_corpora`
                // (the host allow-list) still applies via installed_indexes,
                // so an atom whose corpus isn't allow-listed yields nothing.
                let req_scope = [req.corpus_id.clone()];
                let fts_hits = self
                    // Article slug + passage preview as FTS query
                    // (see eval-side runner.rs comment). Title-bias
                    // pulls intended-article chunks into the pool.
                    .search_corpus_indexes_with_overrides(
                        &[],
                        &format!("{} {}", req.article_slug, req.passage_preview),
                        30,
                        "AtlasNavigate",
                        None,
                        Some(&req_scope),
                        corpus_ceiling,
                    )
                    .await;
                for hit in fts_hits {
                    let title_match = hit
                        .title
                        .as_deref()
                        .map(|t| t == req.article_slug)
                        .unwrap_or(false);
                    if !title_match {
                        continue;
                    }
                    let key = format!(
                        "{}|{}",
                        hit.title.clone().unwrap_or_default(),
                        truncate_chars(&hit.content, 80)
                    );
                    if !seen.insert(key) {
                        continue;
                    }
                    let mut boosted = hit;
                    boosted.score = req.score * 0.05;
                    // Prepend atlas verbatim excerpts harvested from
                    // the atoms that motivated this fetch — concept
                    // defining_quotes and claim quotable_excerpts.
                    // Format `[Atlas highlights] …\n\n<chunk text>`
                    // makes the article's exact words for the
                    // grounded position visible at the head of the
                    // passage. Skips when no excerpts carried (most
                    // atoms have neither field set).
                    if !req.verbatim_excerpts.is_empty() {
                        let mut head = String::from("[Atlas highlights]\n");
                        for ex in &req.verbatim_excerpts {
                            head.push_str(ex);
                            head.push('\n');
                        }
                        head.push('\n');
                        head.push_str(&boosted.content);
                        boosted.content = head;
                    }
                    chunks.push(boosted);
                    graph_added += 1;
                    if graph_added >= fetch_budget {
                        break;
                    }
                }
            }
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
        } else {
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
        }
    }
}
