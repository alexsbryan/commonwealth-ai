// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn __probe` retrieve: embed → hybrid search of the corpus's own indexes
//! (reranked as the runtime is configured) → optional atlas walk and fetch,
//! per question. No retrieval pipeline, so no atlas grounding and no RAPTOR
//! injection; the pool is written out unscored.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use corpus_engine::enrichment::atlas::context_loader::AtlasContextFilter;
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_index::types::ScoredChunk;
use sovereign_contracts::probe::{AtlasProbe, PoolEvidence, ProbeQuestion, SeedMode};
use sovereign_core::atlas_context::{
    atlas_navigate_ann, atlas_top_k_across, cosine, AtlasContext, AtlasGraph,
};

use super::pool_chunk;
use crate::chat_cmd::bootstrap::ChatSession;
use crate::enrich_cmd::paths;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

async fn probe_question(
    session: &ChatSession,
    target_indexes: &[&corpus_index::types::IndexInfo],
    q: &ProbeQuestion,
    limit: usize,
    atlases: &[AtlasContext],
    graphs: &[AtlasGraph],
    seed_mode: SeedMode,
) -> PoolEvidence {
    // 1. Embed.
    let t_embed = Instant::now();
    let embedding = match session.inference.embed_query(&q.question).await {
        Ok(v) => v,
        Err(e) => {
            // An errored row rather than aborting the whole run — one bad
            // question shouldn't void the bank.
            return PoolEvidence {
                id: q.id.clone(),
                error: Some(format!("embed: {e}")),
                chunks: Vec::new(),
                atlas_navigation: Vec::new(),
                embed_ms: t_embed.elapsed().as_millis() as u64,
                search_ms: 0,
                corpora_hit: Vec::new(),
                vector_eligible: false,
                unavailable_corpora: Vec::new(),
                atlas_walk: None,
            };
        }
    };
    let embed_ms = t_embed.elapsed().as_millis() as u64;

    // 2. Compute per-article atlas relevance scores once, before the
    // search loop. These flow into `search_with_rerank` as a third
    // signal alongside the cross-encoder logit and the hybrid fusion
    // score. The map is `article_slug → max_cosine` across every atlas
    // atom (atoms come from many atom types, all carry an embedding).
    // Cheap: ~few thousand cosines per question for the SEP 57-atlas
    // workload. Skipped entirely when the runtime config opts atlas
    // weight to zero (the default) or when no atlases are loaded —
    // baseline-A/B remains byte-equivalent to prior runs.
    let atlas_article_scores: HashMap<String, f32> = if session
        .runtime
        .lane_sources
        .rerank
        .config
        .atlas_weight
        .abs()
        > f32::EPSILON
        && !atlases.is_empty()
        && !embedding.is_empty()
    {
        let mut by_slug: HashMap<String, f32> = HashMap::new();
        for ctx in atlases {
            let slug = ctx
                .atlas_corpus_id
                .strip_prefix("sep-")
                .unwrap_or(&ctx.atlas_corpus_id)
                .to_string();
            let mut best: f32 = 0.0;
            for entry in &ctx.entries {
                let s = cosine(&embedding, &entry.embedding);
                if s > best {
                    best = s;
                }
            }
            if best > 0.0 {
                by_slug
                    .entry(slug)
                    .and_modify(|cur| {
                        if best > *cur {
                            *cur = best;
                        }
                    })
                    .or_insert(best);
            }
        }
        by_slug
    } else {
        HashMap::new()
    };

    // 3. Search every matching corpus index.
    let t_search = Instant::now();
    let mut all_hits: Vec<ScoredChunk> = Vec::new();
    let mut any_vector_eligible = false;
    let mut corpora_hit: Vec<String> = Vec::new();

    for info in target_indexes {
        let dim_match = info.embedding_dimensions == embedding.len();
        if dim_match {
            any_vector_eligible = true;
        }
        let query_vec: &[f32] = if dim_match { &embedding } else { &[] };
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("  open_index({}): {e}", info.corpus_id);
                continue;
            }
        };
        // When atlas grounding is active, pull a wider candidate set
        // from lance so atlas-boost has chunks-from-rank-11-30 to
        // promote into the final top-K. Without this, the boost can
        // only reorder *within* lance's top-K, which is mostly noise
        // (same articles, different positions). With a wider pool,
        // atlas can actually rescue topically-aligned chunks lance
        // ranked just outside the limit.
        let search_limit = if !atlases.is_empty() {
            limit * 3
        } else {
            limit
        };
        // Route through `search_with_rerank` so a runtime-wired
        // cross-encoder reranker actually fires on this path (the
        // eval bypasses `Runtime::search_corpus_indexes`). When no
        // reranker is installed or `rerank_config.enabled = false`,
        // the call is byte-identical to `search()` — same overfetch,
        // same ordering, same threshold semantics — so the
        // baseline-vs-rerank A/B is honest.
        let atlas_scores_opt = if atlas_article_scores.is_empty() {
            None
        } else {
            Some(&atlas_article_scores)
        };
        match idx
            .search_with_rerank(
                query_vec,
                &q.question,
                search_limit,
                session.runtime.lane_sources.rerank.f(),
                &session.runtime.lane_sources.rerank.config,
                atlas_scores_opt,
            )
            .await
        {
            Ok(hits) => {
                if !hits.is_empty() && !corpora_hit.contains(&info.corpus_id) {
                    corpora_hit.push(info.corpus_id.clone());
                }
                all_hits.extend(hits);
            }
            Err(e) => {
                eprintln!("  search({}): {e}", info.corpus_id);
            }
        }
    }
    let search_ms = t_search.elapsed().as_millis() as u64;

    // Atlas-as-graph-navigation. The atlas is a typed knowledge graph
    // (entities, claims, tensions, configurations + edges); cosine-
    // matching individual atom embeddings ("bag-of-atoms") only
    // exercises the most surface-level layer. Real navigation seeds
    // via cosine, then BFS-expands across typed edges (Tension for
    // dialectical pairs, Grounds for argument-depth chains, Involves
    // for entity-event context, Configures for interpretive frame),
    // and identifies the source-chunk neighborhood whose evidence
    // density is highest in the question's atom-vicinity. Those
    // chunks are then fetched via FTS-by-passage_preview against the
    // SEP corpus, restricted to the atom's article.
    let atlas_chunk_requests = if !atlases.is_empty() && !graphs.is_empty() && !embedding.is_empty()
    {
        // Seeds: top-12 atom matches across all atlases (more than
        // the operator's atlas-top-k since seeds drive expansion;
        // many seeds → broader neighborhood).
        let max_seeds = atlases.first().map(|c| c.top_k).unwrap_or(3).max(12);
        let ctx_refs: Vec<&AtlasContext> = atlases.iter().collect();
        let graph_refs: Vec<&AtlasGraph> = graphs.iter().collect();
        // ATLAS_STORAGE_V2 Phase B: the sync `atlas_navigate` is gone — both seed
        // modes now drive the PRODUCTION ANN-seeding navigate over graphs carrying
        // their persistent ANN seed table (attached in `run_bank`); the same code
        // the daemon runs. `--atlas-seed cosine` is kept for runbook compatibility
        // but maps to the identical path.
        match seed_mode {
            SeedMode::Ann | SeedMode::Cosine => {
                atlas_navigate_ann(
                    &q.question,
                    &embedding,
                    &ctx_refs,
                    &graph_refs,
                    max_seeds,
                    /*max_hops=*/ 2,
                )
                .await
            }
        }
    } else {
        Vec::new()
    };

    // The audit-only "atlas_navigation" snapshot — top-K atom matches,
    // unchanged from before, so the JSON output preserves a record of
    // what atlas thinks was relevant. Doesn't enter the prompt.
    let atlas_navigation: Vec<ScoredChunk> = if !atlases.is_empty() && !embedding.is_empty() {
        let nav_k = atlases.first().map(|c| c.top_k).unwrap_or(3);
        let refs: Vec<&AtlasContext> = atlases.iter().collect();
        let nav = atlas_top_k_across(&embedding, &refs, nav_k);
        for c in &nav {
            if !corpora_hit.contains(&c.corpus_id) {
                corpora_hit.push(c.corpus_id.clone());
            }
        }
        nav
    } else {
        Vec::new()
    };

    // Resolve atlas chunk requests via FTS against the SEP corpus.
    // Each ChunkRequest names an article slug + passage_preview; we
    // FTS the preview text against each target index, filter to
    // chunks whose title matches the article, and take the top hit.
    // The atom's aggregated score becomes the chunk's score (boosted
    // significantly to ensure atlas-curated chunks compete with
    // lance vector matches — atlas relevance ~0.6-1.5 vs lance
    // scores ~0.02-0.05). Capped at a budget proportional to limit.
    // Atlas-fetch via question-vector + article-filter. Atlas
    // tells us "this article matters for this question" (via the
    // ChunkRequests' article_slug). Lance has 1024-char chunks for
    // every article in SEP and ranks them by semantic match against
    // the question — but the right article often ranks below top-K
    // when the question only partially mentions it (e.g.
    // communitarianism content for a virtue-ethics question that
    // only names MacIntyre once). Solution: collect unique
    // article-slugs from atlas, then for each do a wide lance
    // search with the question embedding and post-filter to that
    // article's chunks. Returns question-relevant chunks from
    // atlas-aligned articles — lance's specificity meets atlas's
    // article-targeting.
    let atlas_fetch_budget = ((limit as f32) * 0.6).ceil() as usize;
    let mut atlas_fetched: Vec<ScoredChunk> = Vec::new();
    // Internal dedupe for the atlas-fetch loop only — separate from
    // the merge-time dedupe. Using one shared set caused atlas
    // chunks to get rejected at merge because their keys were
    // already in the set from the fetch step.
    let mut atlas_internal_seen: HashSet<String> = HashSet::new();
    let debug_fetch = std::env::var("ATLAS_NAVIGATE_DEBUG").is_ok();

    // Collect unique articles ordered by best (highest) atlas score.
    // Cap article distinct-count at the budget — no point fetching
    // from more articles than we can keep.
    let mut article_score: std::collections::HashMap<String, f32> =
        std::collections::HashMap::new();
    // Aggregate atlas verbatim excerpts (concept defining_quotes,
    // claim quotable_excerpts) per article. We dedupe so the same
    // sentence isn't injected twice when several ChunkRequests for
    // an article overlap in motivating atoms. Injected once per
    // first-chunk-fetched per article in the loop below.
    let mut article_excerpts: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for req in &atlas_chunk_requests {
        let s = article_score
            .entry(req.article_slug().to_string())
            .or_insert(0.0);
        if req.score > *s {
            *s = req.score;
        }
        if !req.verbatim_excerpts.is_empty() {
            let bucket = article_excerpts
                .entry(req.article_slug().to_string())
                .or_default();
            // Three-tier priority for the dialectical_breadth axis:
            //   1. ArgumentReconstructions (`Argument: …`) — densest
            //      P/C structure, best for argument_depth.
            //   2. Contested-tagged Claims (`[… — contested]: …`) —
            //      explicitly counter-position content. Promoting
            //      these into a guaranteed slot is the dialectical
            //      lift this commit targets.
            //   3. Everything else — defining_quotes + regular
            //      quotable_excerpts.
            // Cap raised from 6→8 so all three tiers can land
            // typical entries without one starving another.
            let is_contested = |s: &&String| s.contains("— contested]:");
            let mut prioritised: Vec<&String> = req
                .verbatim_excerpts
                .iter()
                .filter(|s| s.starts_with("Argument:"))
                .collect();
            prioritised.extend(req.verbatim_excerpts.iter().filter(is_contested));
            prioritised.extend(
                req.verbatim_excerpts
                    .iter()
                    .filter(|s| !s.starts_with("Argument:") && !is_contested(s)),
            );
            for ex in prioritised {
                if bucket.len() >= 8 {
                    break;
                }
                if !bucket.iter().any(|existing| existing == ex) {
                    bucket.push(ex.clone());
                }
            }
        }
    }
    let mut articles_ranked: Vec<(String, f32)> = article_score.into_iter().collect();
    articles_ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    articles_ranked.truncate(atlas_fetch_budget);

    if debug_fetch {
        eprintln!(
            "  atlas-fetch: {} unique articles in atlas, fetching from top-{}: {:?}",
            atlas_chunk_requests
                .iter()
                .map(|r| r.article_slug())
                .collect::<HashSet<_>>()
                .len(),
            articles_ranked.len(),
            articles_ranked.iter().map(|(s, _)| s).collect::<Vec<_>>()
        );
    }

    // The wide search below exists solely to serve the atlas-article loop —
    // with no ranked atlas articles (the no-atlas bench path) it would be a
    // full hybrid search per index whose results are thrown away unread.
    let atlas_target_indexes = if articles_ranked.is_empty() {
        &[][..]
    } else {
        target_indexes
    };
    for info in atlas_target_indexes {
        let idx = match session.corpus_engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(_) => continue,
        };
        // One wide lance search reusable across all atlas articles.
        let wide_hits = match idx.search(&embedding, &q.question, 200).await {
            Ok(h) => h,
            Err(_) => continue,
        };
        for (article_slug, atlas_score) in &articles_ranked {
            // Take top 1-2 chunks from this article whose lance
            // ranks against the question are best.
            let mut found = 0usize;
            for hit in &wide_hits {
                if found >= 2 {
                    break;
                }
                if hit.title.as_deref() != Some(article_slug.as_str()) {
                    continue;
                }
                let dedupe_key = format!("{}|{}", article_slug, truncate(&hit.content, 80));
                if !atlas_internal_seen.insert(dedupe_key) {
                    continue;
                }
                let mut boosted = hit.clone();
                // Atlas's contribution: surface chunks lance ranked
                // outside its top-K. Don't boost — let the chunk
                // compete on lance's intrinsic question-relevance
                // score. Atlas adds the chunk to the pool but lance's
                // ranking arbitrates final position. Prevents
                // displacement of specifically-relevant chunks (e.g.
                // ethics-ancient with function-argument detail) by
                // atlas-aligned chunks (communitarianism intro)
                // with weaker question-direct relevance.
                let _ = atlas_score;
                // Inject verbatim atlas excerpts (concept defining
                // sentences + claim quotable_excerpts) on the first
                // chunk fetched for this article. The judge sees the
                // article's exact words for the position the chunk
                // grounds, addressing the 2026-05-06 calibration's
                // "wants direct primary text" finding.
                if found == 0 {
                    if let Some(excerpts) = article_excerpts.get(article_slug) {
                        if !excerpts.is_empty() {
                            let mut head = String::from("[Atlas highlights]\n");
                            for ex in excerpts {
                                head.push_str(ex);
                                head.push('\n');
                            }
                            head.push('\n');
                            head.push_str(&boosted.content);
                            boosted.content = head;
                        }
                    }
                }
                if debug_fetch {
                    eprintln!(
                        "  atlas-fetch: HIT article={} (atlas_score {:.2}, lance {:.4}) → {}",
                        article_slug,
                        atlas_score,
                        hit.score,
                        truncate(&hit.content, 80).replace('\n', " ")
                    );
                }
                atlas_fetched.push(boosted);
                found += 1;
            }
            if found == 0 && debug_fetch {
                eprintln!(
                    "  atlas-fetch: MISS article={} (no chunks with that title in lance top-200)",
                    article_slug
                );
            }
        }
    }

    // Additive atlas merge: lance keeps its full top-`limit` set;
    // atlas-fetched chunks append. Final retrieved is `limit +
    // up-to-atlas_slots` total. This is the "atlas augments,
    // doesn't displace" design — bank-wide reserved-slots showed
    // that swapping lance chunks for atlas chunks at fixed limit=10
    // hurts essay axes (atlas chunks displace lance argument-detail
    // for topical breadth). Additive lets the judge see both:
    // lance's question-direct retrieval AND atlas's article-
    // targeted supplement. Judge prompt grows by ~2K chars per
    // question; well within the fast slot's budget.
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_hits.truncate(limit);

    // Dedup atlas chunks against lance's retained set.
    let mut seen_chunks: HashSet<String> = HashSet::new();
    for hit in &all_hits {
        let dedupe_key = format!(
            "{}|{}",
            hit.title.clone().unwrap_or_default(),
            truncate(&hit.content, 80)
        );
        seen_chunks.insert(dedupe_key);
    }
    let pre_atlas_count = all_hits.len();
    let mut atlas_added_in = 0usize;
    for af in atlas_fetched.iter() {
        if atlas_added_in >= atlas_fetch_budget {
            break;
        }
        let dedupe_key = format!(
            "{}|{}",
            af.title.clone().unwrap_or_default(),
            truncate(&af.content, 80)
        );
        if seen_chunks.insert(dedupe_key) {
            all_hits.push(af.clone());
            atlas_added_in += 1;
        }
    }
    if pre_atlas_count != all_hits.len() {
        eprintln!(
            "  atlas-navigate: BFS produced {} requests; {} fetched, \
             {} appended (additive: {} lance + {} atlas = {} total)",
            atlas_chunk_requests.len(),
            atlas_fetched.len(),
            all_hits.len() - pre_atlas_count,
            limit,
            atlas_added_in,
            all_hits.len(),
        );
    }

    PoolEvidence {
        id: q.id.clone(),
        error: None,
        chunks: all_hits.iter().map(pool_chunk).collect(),
        atlas_navigation: atlas_navigation.iter().map(pool_chunk).collect(),
        embed_ms,
        search_ms,
        corpora_hit,
        vector_eligible: any_vector_eligible,
        unavailable_corpora: Vec::new(),
        atlas_walk: None,
    }
}

/// Search `corpus` for every question, sequentially. Sequential is fine —
/// the daemon's embed slot serialises anyway, and concurrent searches
/// against the same Lance table contend on the same index pages.
pub(crate) async fn probe(
    session: &ChatSession,
    corpus: &str,
    questions: &[ProbeQuestion],
    limit: usize,
    atlases: &[AtlasContext],
    graphs: &[AtlasGraph],
    seed_mode: SeedMode,
) -> Result<Vec<PoolEvidence>, String> {
    let indexes = session
        .corpus_engine
        .installed_indexes()
        .await
        .map_err(|e| format!("installed_indexes(): {e}"))?;

    if indexes.is_empty() {
        return Err(format!(
            "no corpora installed — `svrn corpus install {corpus}` before running this bank"
        ));
    }

    let target_indexes: Vec<_> = indexes
        .iter()
        .filter(|info| info.corpus_id == corpus)
        .collect();

    if target_indexes.is_empty() {
        let installed: Vec<&str> = indexes.iter().map(|i| i.corpus_id.as_str()).collect();
        return Err(format!(
            "bank corpus `{corpus}` is not installed. Installed: {installed:?}"
        ));
    }

    // ATLAS_STORAGE_V2 3b: when `--atlas-seed ann`, attach each corpus's
    // PERSISTENT ANN seed table (built by `svrn atlas backfill-ann`) to its
    // graph, then drive the PRODUCTION `atlas_navigate_ann` over the
    // ann-attached graphs — the daemon's exact runtime shape, not a fork. The
    // owned graphs must outlive the per-question loop (each holds a live
    // `lancedb::Table` opened on THIS runtime; see
    // `open_and_attach_ann_seed_table`).
    let ann_graphs: Vec<AtlasGraph> = if seed_mode == SeedMode::Ann {
        let mut out = Vec::with_capacity(graphs.len());
        let mut attached = 0usize;
        for g in graphs {
            let atlas_dir = paths::index_root(&g.atlas_corpus_id).join(ATLAS_DIRNAME);
            let g = sovereign_core::atlas_context::open_and_attach_ann_seed_table(
                &g.atlas_corpus_id,
                &atlas_dir,
                g.clone(),
            )
            .await;
            if g.has_ann_seed_table() {
                attached += 1;
            }
            out.push(g);
        }
        eprintln!(
            "atlas-seed=ann: attached persistent ANN seed tables to {attached}/{} graphs \
             (run `svrn atlas backfill-ann <corpus>` for any missing)",
            out.len()
        );
        out
    } else {
        Vec::new()
    };
    let graphs: &[AtlasGraph] = if seed_mode == SeedMode::Ann {
        &ann_graphs
    } else {
        graphs
    };

    let mut rows = Vec::with_capacity(questions.len());
    for q in questions {
        rows.push(
            probe_question(
                session,
                &target_indexes,
                q,
                limit,
                atlases,
                graphs,
                seed_mode,
            )
            .await,
        );
    }
    Ok(rows)
}

/// Load an atlas corpus's embedded context bag through the ONE loader,
/// `corpus_engine::enrichment::atlas::context_loader::load_atlas_context` (ontology-v1
/// P0.2 moved the body there so the daemon can seed a fresh atlas
/// in-process). This wrapper supplies only what the CLI has and the library
/// must not assume: the session's inference provider and the atlas dir under
/// the enrichment store.
pub async fn load_atlas_context(
    session: &ChatSession,
    atlas_corpus_id: &str,
    top_k: usize,
    filter: &AtlasContextFilter,
) -> Result<AtlasContext, String> {
    let atlas_dir = paths::index_root(atlas_corpus_id).join(ATLAS_DIRNAME);
    let embed = sovereign_core::embed_fn::inference_to_embed_query_fn(session.inference.clone());
    corpus_engine::enrichment::atlas::context_loader::load_atlas_context(
        &embed,
        &atlas_dir,
        atlas_corpus_id,
        top_k,
        filter,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Load every atlas the request names: its embedded context bag, and the
/// structural graph layer (atoms-by-id, edge adjacency) `atlas_navigate`
/// walks. A bag that will not load fails the run; a graph that will not load
/// is a warning, and that atlas is walked by cosine alone.
pub(crate) async fn load_atlases(
    session: &ChatSession,
    atlas: &AtlasProbe,
) -> Result<(Vec<AtlasContext>, Vec<AtlasGraph>), String> {
    let include_claims = atlas.include_kinds.iter().any(|k| k == "claim");
    let include_tensions = atlas.include_kinds.iter().any(|k| k == "tension");
    let include_configurations = atlas.include_kinds.iter().any(|k| k == "configuration");
    // Surface unknown kinds as a warning so typos don't silently
    // produce an entities-only run.
    for k in &atlas.include_kinds {
        if !matches!(k.as_str(), "claim" | "entity" | "tension" | "configuration") {
            eprintln!(
                "warn: --atlas-include `{k}` is not yet recognised; \
             accepted today: entity, claim, tension, configuration."
            );
        }
    }
    // The filter the eval harness applies is the SAME type the grounding
    // path uses, and `atlas_min_description_chars` now defaults to that
    // type's own floor (nc-22c found them diverged: 200 here vs 10 there,
    // so eval measured an atom universe production had abandoned).
    // Closed 2026-08-21 by operator decision — it moves published eval
    // numbers, which is why it was not a drive-by (ARCH §18.6).
    let filter = AtlasContextFilter {
        min_description_chars: atlas.min_description_chars,
        depth_allowlist: atlas.depth_allowlist.clone(),
        max_entries: atlas.max_entries,
        include_claims,
        include_tensions,
        include_configurations,
        ..AtlasContextFilter::default()
    };
    // `--with-atlas` accepts a comma-separated list of atlas
    // corpus ids. Each loads independently (with its own
    // canonical_name = article_slug derivation) and the per-question
    // retrieval pools their entries via `atlas_top_k_across`. This
    // is the multi-article SEP-pilot path: enrich N per-article
    // atlases, point one --with-atlas at all of them, let the
    // global cosine pick the topically-aligned surfaces.
    let mut ctxs = Vec::new();
    for id in &atlas.corpus_ids {
        match load_atlas_context(session, id, atlas.top_k, &filter).await {
            Ok(ctx) => ctxs.push(ctx),
            Err(e) => return Err(format!("--with-atlas {id}: {e}")),
        }
    }

    // Load the structural graph layer for each atlas (atoms-by-id,
    // edge adjacency). Used by `atlas_navigate` for graph BFS — the
    // substantive layer of the atlas that bag-of-atoms cosine
    // retrieval ignores. Cheap: just parses atoms.json + edges.json
    // already on disk from build time.
    let mut graphs = Vec::with_capacity(ctxs.len());
    for ctx in &ctxs {
        let atlas_dir = paths::index_root(&ctx.atlas_corpus_id).join(ATLAS_DIRNAME);
        // ATLAS_STORAGE_V2: the AtlasGraph is the v2 store (atoms.lance +
        // edges.csr), read through the production direct-read backend — the
        // same reader the daemon uses (atoms resident + edges.csr mmap).
        match AtlasGraph::load_from_disk(
            &ctx.atlas_corpus_id,
            &atlas_dir,
            corpus_engine::enrichment::atlas::context::read_section_rows(&atlas_dir),
        ) {
            Ok(g) => graphs.push(g),
            Err(e) => eprintln!("warn: atlas-graph load `{}`: {e}", ctx.atlas_corpus_id),
        }
    }
    Ok((ctxs, graphs))
}
