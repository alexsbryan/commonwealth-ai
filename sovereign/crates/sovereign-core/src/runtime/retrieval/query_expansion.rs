// SPDX-License-Identifier: AGPL-3.0-or-later
//! Query-side expansion: axis-aware Wikipedia-graph neighbor
//! expansion, heuristic question decomposition + sub-query
//! fan-out, and Fast-slot title expansion.

use super::super::*;

use crate::runtime::evidence::{extract_tokens, EVIDENCE_TITLE_MIN_TOKEN_LEN};

impl Runtime {
    /// Axis-aware structural-graph expansion (opt-in via
    /// `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND=1`). Two primitives:
    ///
    /// 1. **Per-entity axis-aligned neighbors**: for each entity in
    ///    the question, return outbound graph edges whose target
    ///    title, link text, or section path lexically match the
    ///    question's axis term(s). Surfaces concept articles the
    ///    entity directly references that are about the asked
    ///    dimension.
    /// 2. **Co-citation**: for ≥2-entity questions, articles linked
    ///    to by all entities (intersection of outbound edge sets),
    ///    optionally axis-filtered. Surfaces bridge concepts that
    ///    a comparative answer would naturally cite.
    ///
    /// When the question has no extractable axis or only one
    /// entity, falls back to occurrence-ranked neighbors of the
    /// top hit. Neighbor chunks are score-decayed by
    /// [`GRAPH_NEIGHBOR_DECAY`] so they compete with original hits
    /// only after query-relevance reweighting promotes them.
    pub(crate) async fn expand_via_wikipedia_graph(
        &self,
        chunks: &[corpus_engine::ScoredChunk],
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        if std::env::var("SOVEREIGN_GRAPH_NEIGHBOR_EXPAND")
            .ok()
            .as_deref()
            != Some("1")
        {
            return None;
        }
        let graph = self.wikipedia_graph.as_ref()?;

        let already_present: std::collections::HashSet<String> =
            chunks.iter().filter_map(|c| c.title.clone()).collect();

        // Pull entities + axis from the question. The comparison
        // extractor is broader than the proper-noun one (catches
        // lowercase contrast pairs) so it's the right primary.
        let entities = extract_comparison_entities(message);
        let axis = comparison_axis(message, &entities);

        // Build the axis-term vocabulary for graph filtering. We
        // include both the joined axis phrase and its individual
        // ≥4-char tokens so a multi-word axis like "salvation and
        // the afterlife" matches "Salvation in Christianity" and
        // "Eternal salvation" simultaneously.
        let axis_terms: Vec<String> = if let Some(axis) = axis.as_ref() {
            let mut v: Vec<String> = vec![axis.clone()];
            for tok in axis.split_whitespace() {
                let t = tok
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase();
                if t.len() >= 4 && !["with", "their", "from", "into", "onto"].contains(&t.as_str())
                {
                    v.push(t);
                }
            }
            v
        } else {
            Vec::new()
        };

        let mut candidate_titles: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Primitive 1: per-entity axis-aligned neighbors.
        if !axis_terms.is_empty() {
            for entity in &entities {
                for n in graph
                    .neighbors_for_axis(entity, &axis_terms, GRAPH_NEIGHBORS_PER_HIT)
                    .await
                {
                    if n.in_scope && !already_present.contains(&n.title) {
                        candidate_titles.insert(n.title);
                    }
                }
            }
        }

        // Primitive 2: co-citation across all entities. Empty axis
        // is allowed — the intersection is itself a strong filter.
        if entities.len() >= 2 {
            for n in graph
                .co_neighbors(&entities, &axis_terms, GRAPH_NEIGHBORS_PER_HIT)
                .await
            {
                if n.in_scope && !already_present.contains(&n.title) {
                    candidate_titles.insert(n.title);
                }
            }
        }

        // Fallback: no axis + single-entity → use occurrence-ranked
        // neighbors of the top hit. Cheap, preserves the prior
        // behaviour for shapes the new primitives don't address.
        if candidate_titles.is_empty() {
            if let Some(top_title) = chunks.iter().find_map(|c| c.title.as_ref()) {
                for n in graph.neighbors(top_title, GRAPH_NEIGHBORS_PER_HIT).await {
                    if n.in_scope && !already_present.contains(&n.title) {
                        candidate_titles.insert(n.title);
                    }
                }
            }
        }

        if candidate_titles.is_empty() {
            return Some(Vec::new());
        }

        eprintln!(
            "[graph_expand] entities={:?} axis={:?} candidates={:?}",
            entities,
            axis,
            candidate_titles.iter().collect::<Vec<_>>(),
        );

        // Title-anchored retrieval per candidate. Filter to chunks
        // whose title matches the candidate exactly to avoid
        // cross-corpus false positives that share a token.
        let parent_score: f32 = chunks.first().map(|c| c.score).unwrap_or(0.05);
        let mut added: Vec<corpus_engine::ScoredChunk> = Vec::new();
        for title in candidate_titles {
            let title_emb = self.inference.embed_query(&title).await.unwrap_or_default();
            let hits = self
                .search_corpus_indexes_with_overrides(
                    &title_emb,
                    &title,
                    GRAPH_NEIGHBOR_LIMIT,
                    "GraphExpand",
                    None,
                    enabled_corpora,
                    corpus_ceiling,
                )
                .await;
            for mut c in hits {
                if c.title.as_deref() != Some(title.as_str()) {
                    continue;
                }
                c.score = (c.score * GRAPH_NEIGHBOR_DECAY).max(parent_score * GRAPH_NEIGHBOR_DECAY);
                added.push(c);
            }
        }
        let chunks_added = added.len();
        tracing::debug!(
            entities = ?entities,
            axis = ?axis,
            chunks_added,
            query = message,
            "graph axis-aware expansion: completed"
        );
        Some(added)
    }
    /// PPR structural expansion with a cross-encoder admission gate
    /// (opt-in via `SOVEREIGN_PPR_EXPAND=1`; requires both a wikipedia
    /// graph and an installed `rerank_fn`).
    ///
    /// The S3 probes (RETRIEVAL_REDESIGN.md, 2026-07-16) established
    /// that structural candidates admitted on title-cosine displace
    /// fact-bearing direct hits past the truncate — even humble
    /// injection netted −6 facts for +1 source. The unlock is the S4
    /// admission gate: a cross-encoder judges each structural
    /// candidate against the query, and a candidate is injected ONLY
    /// when it out-scores the marginal direct hits it would displace
    /// (the displacement test) AND clears the model's absolute
    /// relevance floor (yes-logit > no-logit, i.e. score > 0).
    ///
    /// Proposal is two channels; disposal is the gate:
    ///
    /// 1. **Walk (channel A)** — forward-push personalized PageRank
    ///    over the wikipedia link graph. Seeds = question entities +
    ///    top in-pool titles; two push rounds weighted by link
    ///    occurrence counts. Catches aboutness-adjacent articles
    ///    (bridges, co-cited concepts).
    /// 2. **Typed edges (channel B)** — the seeds' `causal` +
    ///    `contested` outbound edges, unconditionally. Edge types are
    ///    classified at insert time from section paths ("Origins",
    ///    "Criticism", …) and link-text verbs, so these are the rare
    ///    edges that carry answer-side people/causes a question never
    ///    names (measured: Manhattan Project → Szilard/Fermi/Einstein
    ///    are ALL `causal` edges from its Origins section, at
    ///    occurrence weight 1 in a 508-neighbor article — mass alone
    ///    structurally cannot surface them, and 2026-07-16's S3
    ///    probes showed lexical admission must not).
    /// 3. **Title prerank** — when the merged candidate pool exceeds
    ///    the fetch budget, one batched rerank call over bare titles
    ///    (~15ms/pair) keeps the top few by semantic judgment.
    /// 4. **Fetch** — FTS-only title-anchored retrieval per surviving
    ///    candidate (title-only query; empty embedding ⇒ no ANN).
    /// 5. **Gate** — one batched rerank call scores candidate chunks
    ///    against the model's absolute yes/no floor. Admitted chunks
    ///    are placed mid-pool via a synthetic `vector_distance` (the
    ///    merged sort orders by distance; `None` would sort them
    ///    straight out, and tail placement made them the downstream
    ///    expander's first eviction).
    ///
    /// # Concurrency shape (promotion, 2026-07-17)
    ///
    /// The lane is POOL-INDEPENDENT except at its edges — seeds are a
    /// spawn-time snapshot (entities + top pool titles) and placement
    /// needs only the join-time pool — so it runs as a spawned task
    /// overlapping the core pipeline steps (atlas/RAPTOR grounding,
    /// reweight): `spawn_ppr_lane` right after `entity_boost`,
    /// join + placement at the old step position. Measured serial
    /// cost was ~1.3-1.9s per question; overlapped, the wall cost is
    /// max(0, lane − core), near zero on knowledge-query turns.
    pub(crate) fn spawn_ppr_lane(
        &self,
        chunks: &[corpus_engine::ScoredChunk],
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Option<tokio::task::JoinHandle<Vec<corpus_engine::ScoredChunk>>> {
        // Default ON (promoted 2026-07-17 after the v10 battery:
        // +2 wiki sources, facts held, +182ms p50 = harness noise;
        // "0"/"false"/"off"/"no" disables — same convention as
        // SOVEREIGN_ATLAS_GROUNDING). Boxes without a reranker
        // no-op a few lines down.
        if let Ok(v) = std::env::var("SOVEREIGN_PPR_EXPAND") {
            if matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            ) {
                return None;
            }
        }
        let graph = self.wikipedia_graph.clone()?;
        let engine = self.corpus_engine.clone()?;
        let Some(rerank_fn) = self.rerank_fn.clone() else {
            // The gate IS the feature — ungated structural injection
            // is a measured regression. No reranker ⇒ the lane stays
            // dark (debug, not warn: the flag defaults ON and most
            // installs have no reranker model yet).
            tracing::debug!(
                "ppr_expand: no rerank_fn installed — lane dark \
                 (set SOVEREIGN_RERANK_MODEL_PATH, optionally with \
                 SOVEREIGN_RERANK_GATE_ONLY=1)"
            );
            return None;
        };
        if chunks.is_empty() {
            return None;
        }
        let lane = PprLane {
            graph,
            engine,
            rerank_fn,
            gliner: self.gliner.clone(),
        };
        let pool_titles: Vec<String> = chunks.iter().filter_map(|c| c.title.clone()).collect();
        let message = message.to_string();
        let enabled = enabled_corpora.map(|s| s.to_vec());
        let ceiling = corpus_ceiling.map(|s| s.to_vec());
        Some(tokio::spawn(ppr_propose_and_gate(
            lane,
            pool_titles,
            message,
            enabled,
            ceiling,
        )))
    }
}

/// The owned components the spawned PPR lane needs — all cheap Arc
/// clones, so the task runs detached from the pipeline borrow.
pub(crate) struct PprLane {
    pub graph: std::sync::Arc<dyn corpus_engine::WikipediaGraphApi>,
    pub engine: std::sync::Arc<corpus_engine::CorpusEngine>,
    pub rerank_fn: corpus_engine::RerankFn,
    pub gliner: Option<std::sync::Arc<dyn crate::traits::EntityExtractor>>,
}

impl Runtime {
    /// Spawn the entity-obligations fetch (the SUPPLY half of the
    /// merge-select architecture; gated with it). Question-named
    /// entities become fetch OBLIGATIONS — title-resolved and
    /// title-fetched directly — instead of embedding-search hints:
    /// the bucket-1 forensic (2026-07-17) showed the named entity's
    /// canonical chunks usually never entered the pool at all, so no
    /// merge-side ordering could recover them. Pool-independent, so
    /// it overlaps the core steps exactly like the PPR lane.
    pub(crate) fn spawn_entity_obligations(
        &self,
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Option<tokio::task::JoinHandle<Vec<corpus_engine::ScoredChunk>>> {
        if !merge_select_enabled() {
            return None;
        }
        let engine = self.corpus_engine.clone()?;
        let rerank_fn = self.rerank_fn.clone();
        let message = message.to_string();
        let enabled = enabled_corpora.map(|s| s.to_vec());
        let ceiling = corpus_ceiling.map(|s| s.to_vec());
        // GLiNER concept pass runs inside the spawned task (off the
        // pipeline setup path) to surface lowercase concept articles the
        // uppercase-only heuristic can't. `None` when the model isn't
        // installed → no concept obligations, unchanged behavior.
        let gliner = self.gliner.clone();
        Some(tokio::spawn(fetch_entity_obligations(
            engine, rerank_fn, message, enabled, ceiling, gliner,
        )))
    }
}

/// Resolve each question-named entity to its best-matching article
/// title(s) via title-FTS, pull the articles wholesale
/// (`fetch_chunks_by_title` — BTree-indexed), keep the top chunks by
/// substantive question-token overlap, and tag them
/// (`obligation_entity`) for the merge selector's demand slots.
pub(crate) async fn fetch_entity_obligations(
    engine: std::sync::Arc<corpus_engine::CorpusEngine>,
    rerank_fn: Option<corpus_engine::RerankFn>,
    message: String,
    enabled_corpora: Option<Vec<String>>,
    corpus_ceiling: Option<Vec<String>>,
    gliner: Option<std::sync::Arc<dyn crate::traits::EntityExtractor>>,
) -> Vec<corpus_engine::ScoredChunk> {
    let mut entities: Vec<String> = extract_comparison_entities(&message);
    if entities.is_empty() {
        entities = extract_question_entities(&message);
    }
    entities.truncate(MAX_ENTITY_QUERIES);

    // Work list of (surface term, is_concept). Named entities come from
    // the uppercase-only heuristic; concepts come from the GLiNER
    // `Concept` pass and cover the lowercase abstract nouns the
    // heuristic structurally cannot see ("determinism", "colonialism",
    // "uncertainty principle"). Concepts are tagged so resolution can
    // use bidirectional title matching (see below); everything else
    // about the fetch is identical.
    let mut work: Vec<(String, bool)> = entities.into_iter().map(|e| (e, false)).collect();
    if let Some(g) = gliner.as_ref().filter(|_| concept_obligations_enabled()) {
        let mut added = 0usize;
        for c in g.extract_concepts(&message) {
            if added >= MAX_CONCEPT_QUERIES {
                break;
            }
            // Skip a concept only when a named entity ALREADY covers it
            // (equal, or the named span contains the concept). Do NOT
            // skip when the concept merely contains a named entity: a
            // junk short entity like "European" is a substring of the
            // real concept "European colonialism", and the two resolve
            // to DIFFERENT articles ("European long-distance paths" vs
            // "Colonialism") — dropping the concept there was the bug
            // that starved the colonialism source (receipt 2026-07-17).
            let cl = c.to_lowercase();
            if work.iter().any(|(t, _)| {
                let tl = t.to_lowercase();
                tl == cl || tl.contains(&cl)
            }) {
                continue;
            }
            work.push((c, true));
            added += 1;
        }
    }
    if work.is_empty() {
        return Vec::new();
    }

    let Ok(installed) = engine.installed_indexes().await else {
        return Vec::new();
    };
    let paths: Vec<std::path::PathBuf> = installed
        .into_iter()
        .filter(|i| {
            matches!(
                i.kind,
                corpus_engine::CorpusKind::Knowledge | corpus_engine::CorpusKind::Catalog
            ) && enabled_corpora
                .as_deref()
                .map(|e| e.iter().any(|c| c == &i.corpus_id))
                .unwrap_or(true)
                && corpus_ceiling
                    .as_deref()
                    .map(|e| e.iter().any(|c| c == &i.corpus_id))
                    .unwrap_or(true)
        })
        .map(|i| i.path)
        .collect();
    if paths.is_empty() {
        return Vec::new();
    }

    let q_tokens = extract_tokens(&message, EVIDENCE_TITLE_MIN_TOKEN_LEN);
    let t_start = std::time::Instant::now();
    let fetches = work.iter().map(|(entity, is_concept)| {
        let entity = entity.clone();
        let is_concept = *is_concept;
        let paths = paths.clone();
        let engine = engine.clone();
        let q_tokens = q_tokens.clone();
        let rerank_fn = rerank_fn.clone();
        let message = message.clone();
        async move {
            let e_lower = entity.to_lowercase();
            let mut out: Vec<corpus_engine::ScoredChunk> = Vec::new();
            for path in &paths {
                let Ok(idx) = engine.open_index(path).await else {
                    continue;
                };
                // Title resolution: FTS-only search on the entity's
                // surface form; keep distinct titles CONTAINING it
                // ("Newton" → "Isaac Newton", "Newton's law of …").
                let hits = idx.search(&[], &entity, OBLIGATION_RESOLVE_LIMIT).await;
                let Ok(hits) = hits else { continue };
                #[allow(unused_mut)]
                let mut candidates: Vec<String> = Vec::new();
                for h in hits {
                    let Some(t) = h.title else { continue };
                    let tl = t.to_lowercase();
                    // Named entities keep titles that CONTAIN the surface
                    // form (the entity is usually shorter than its
                    // canonical title: "Newton" ⊂ "Isaac Newton"). A
                    // concept's canonical title is often SHORTER than the
                    // extracted span ("Colonialism" ⊂ "European
                    // colonialism"), so concepts additionally accept
                    // titles the entity contains — bidirectional match,
                    // still exact-title-first below.
                    let keep = if is_concept {
                        tl.contains(&e_lower) || e_lower.contains(&tl)
                    } else {
                        tl.contains(&e_lower)
                    };
                    if keep && !candidates.contains(&t) {
                        candidates.push(t);
                    }
                }
                // Disambiguate toward the CANONICAL article. Title-
                // contains alone measured wrong: entity "Newton"
                // resolved to 'Newton (unit)' / "Newton's method" /
                // 'Arik Einstein' while 'Isaac Newton' sat unused.
                // The cross-encoder judging candidate TITLES against
                // the QUESTION is the same prerank the PPR lane
                // proved (~30ms/pair, overlapped); without a
                // reranker: exact match first, then shortest title
                // (canonical bios are short; qualified variants are
                // long).
                // An EXACT title match IS the canonical resolution —
                // never let the CE second-guess it (measured: it
                // preferred 'Eastern Christianity' over the exact
                // 'Christianity' for a Buddhism-vs-Christianity
                // question, flooding the pool with sibling-article
                // chunks that displaced fact-bearing depth).
                if let Some(exact) = candidates
                    .iter()
                    .position(|t| t.eq_ignore_ascii_case(&entity))
                {
                    let t = candidates.swap_remove(exact);
                    candidates.clear();
                    candidates.push(t);
                }
                let titles: Vec<String> = if candidates.len() <= OBLIGATION_TITLES_PER_ENTITY {
                    candidates
                } else if let Some(rf) = rerank_fn.as_ref() {
                    match (rf)(&message, candidates.clone()).await {
                        Ok(scores) if scores.len() == candidates.len() => {
                            let mut order: Vec<usize> = (0..candidates.len()).collect();
                            order.sort_by(|&a, &b| {
                                scores[b]
                                    .partial_cmp(&scores[a])
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            });
                            order
                                .into_iter()
                                .take(OBLIGATION_TITLES_PER_ENTITY)
                                .map(|i| candidates[i].clone())
                                .collect()
                        }
                        _ => candidates
                            .into_iter()
                            .take(OBLIGATION_TITLES_PER_ENTITY)
                            .collect(),
                    }
                } else {
                    let mut c = candidates;
                    c.sort_by_key(|t| {
                        (
                            !t.eq_ignore_ascii_case(&entity), // exact first
                            t.len(),                          // then shortest
                        )
                    });
                    c.truncate(OBLIGATION_TITLES_PER_ENTITY);
                    c
                };
                for title in titles {
                    let Ok(mut article) = idx
                        .fetch_chunks_by_title(&title, PPR_ARTICLE_FETCH_LIMIT)
                        .await
                    else {
                        continue;
                    };
                    let overlap = |c: &corpus_engine::ScoredChunk| -> usize {
                        let body = c.content.to_lowercase();
                        q_tokens
                            .iter()
                            .filter(|t| body.contains(t.as_str()))
                            .count()
                    };
                    article.sort_by_key(|c| std::cmp::Reverse(overlap(c)));
                    article.truncate(OBLIGATION_CHUNKS_PER_TITLE);
                    for mut c in article {
                        c.metadata
                            .insert("obligation_entity".to_string(), entity.clone());
                        out.push(c);
                    }
                }
                if !out.is_empty() {
                    break; // first corpus that resolves the entity wins
                }
            }
            out
        }
    });
    let obligations: Vec<corpus_engine::ScoredChunk> =
        futures::future::join_all(fetches).await.concat();
    // Glassbox: name the named entities AND the concept articles that
    // fired separately, so a reader of the audit trail can see exactly
    // which lowercase concepts the GLiNER pass contributed this turn.
    let named: Vec<&String> = work.iter().filter(|(_, c)| !c).map(|(t, _)| t).collect();
    let concepts: Vec<&String> = work.iter().filter(|(_, c)| *c).map(|(t, _)| t).collect();
    tracing::info!(
        target: "retrieval_audit",
        event = "entity_obligations",
        entities = ?named,
        concepts = ?concepts,
        fetched = obligations.len(),
        ms = t_start.elapsed().as_millis() as u64,
        "retrieval_audit: entity_obligations"
    );
    obligations
}

/// Walk → typed edges → title prerank → fetch → chunk gate. Returns
/// ADMITTED chunks (cross-encoder metadata attached) with NO placement
/// — the join step assigns `vector_distance` against the live pool.
pub(crate) async fn ppr_propose_and_gate(
    lane: PprLane,
    pool_titles: Vec<String>,
    message: String,
    enabled_corpora: Option<Vec<String>>,
    corpus_ceiling: Option<Vec<String>>,
) -> Vec<corpus_engine::ScoredChunk> {
    {
        let graph = &lane.graph;
        let message = message.as_str();
        let enabled_corpora = enabled_corpora.as_deref();
        let corpus_ceiling = corpus_ceiling.as_deref();
        let rerank_fn = lane.rerank_fn.clone();

        let already_present: std::collections::HashSet<&str> =
            pool_titles.iter().map(|t| t.as_str()).collect();

        // ── Phase 1: seeds + forward-push walk ──────────────────────
        let t_walk = std::time::Instant::now();
        let mut t_sub = std::time::Instant::now();
        let mut sub_extract_ms = 0u64;
        let mut sub_gliner_ms = 0u64;
        let mut sub_record_ms = 0u64;
        let mut sub_pulls_ms = 0u64;
        let mut sub_hops_ms = 0u64;
        let mut seeds: Vec<String> = extract_comparison_entities(message);
        if seeds.is_empty() {
            seeds = extract_question_entities(message);
        }
        sub_extract_ms = t_sub.elapsed().as_millis() as u64;
        t_sub = std::time::Instant::now();
        // GLiNER confirmation filter (when the extractor is wired):
        // keep only heuristic seeds the NER model also sees as
        // entities. The heuristics over-trigger on capitalized
        // non-entities (measured 2026-07-17: "European Union" seeded
        // from "European physicists", "Western world" from a Rome
        // question) and every junk seed wastes walk mass on a wrong
        // branch. GLiNER output is lower-cased so it can't seed the
        // case-sensitive graph directly — it CONFIRMS cased spans
        // instead. Substring match in both directions covers
        // span-boundary differences ("nazi germany" vs "Germany").
        if let Some(g) = lane.gliner.as_ref() {
            let ner: Vec<String> = g.extract_entities(message);
            if !ner.is_empty() {
                let confirmed: Vec<String> = seeds
                    .iter()
                    .filter(|s| {
                        let sl = s.to_lowercase();
                        ner.iter()
                            .any(|e| sl == *e || sl.contains(e.as_str()) || e.contains(sl.as_str()))
                    })
                    .cloned()
                    .collect();
                if !confirmed.is_empty() && confirmed.len() < seeds.len() {
                    tracing::debug!(
                        dropped = seeds.len() - confirmed.len(),
                        ?confirmed,
                        "ppr_expand: gliner seed confirmation"
                    );
                    seeds = confirmed;
                }
            }
        }
        sub_gliner_ms = t_sub.elapsed().as_millis() as u64;
        t_sub = std::time::Instant::now();
        for title in pool_titles.iter() {
            if seeds.len() >= PPR_MAX_SEEDS {
                break;
            }
            if !seeds.iter().any(|s| s == title) {
                seeds.push(title.clone());
            }
        }
        // Keep only seeds the graph knows; a dropped seed is fine
        // (its mass just never enters the walk).
        let mut live_seeds: Vec<String> = Vec::new();
        for s in &seeds {
            if live_seeds.len() >= PPR_MAX_SEEDS {
                break;
            }
            if graph.record(s).await.is_some() {
                live_seeds.push(s.clone());
            }
        }
        sub_record_ms = t_sub.elapsed().as_millis() as u64;
        t_sub = std::time::Instant::now();
        if live_seeds.is_empty() {
            return Vec::new();
        }

        // rank = accumulated PPR mass per article; mass = the frontier
        // being pushed this round. Neighbor edges are weighted by
        // occurrence count; PPR_DAMPING of a node's mass is pushed,
        // the rest is retained where it sits (already in `rank`).
        //
        // The seed hop pulls each seed's adjacency WIDE (one sqlite
        // query each, in `live_seeds` order — entity seeds first) so
        // the same rows serve both channels: the top slice feeds the
        // mass push, and the `causal`/`contested` typed rows become
        // channel-B candidates under a PER-SEED quota. The quota is
        // the load-bearing detail: iteration 2 collected typed edges
        // in frontier order — a HashMap, i.e. ARBITRARY seed order —
        // and one edge-dense seed (Nazism's Criticism section) filled
        // the whole cap before the question's primary entity was even
        // visited.
        let seed_mass = 1.0 / live_seeds.len() as f64;
        let mut rank: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        let mut mass: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        let mut in_scope: std::collections::HashMap<String, bool> =
            std::collections::HashMap::new();
        // (candidate title, seed article, relationship type) — the
        // provenance rides to the GATE, where the candidate's doc is
        // scored WITH its bridge context (BridgeRAG, arXiv:2604.03384:
        // later-hop evidence must be judged conditioned on the bridge,
        // not on similarity to the original question — the measured
        // reason answer-side people score -1..-3 under a bare
        // does-this-answer framing).
        let mut typed: Vec<(String, String, String)> = Vec::new();
        // Funnel-widening (2026-07-17, graph reconnaissance): EVERY
        // measured proposal-gap miss is one hop from the seeds —
        // Watt/Steam engine/Enclosure are causal-typed (lost the old
        // per-seed quota draw), Bohr/Heisenberg are causal from the
        // seed the quota starved, Hahn/Nirvana/Bastille/Weimar are
        // TOPICAL edges the typed filter never saw. Occurrence
        // weights carry no signal at w∈{1,2}, so pre-cutting by
        // quota/occurrence was a lottery. The funnels now stay wide
        // and the title-CE prerank (semantic, measured ranking
        // Fermi/Wigner/Einstein top-4) does ALL the picking.
        for s in &live_seeds {
            rank.insert(s.clone(), seed_mass);
            mass.insert(s.clone(), seed_mass);
            in_scope.insert(s.clone(), true);
        }
        let mut seed_pulls: std::collections::HashMap<
            String,
            Vec<corpus_engine::WikipediaNeighbor>,
        > = std::collections::HashMap::new();
        // The wide pulls are independent Lance point-queries —
        // sequential they cost ~200ms × seeds (measured pulls=1042ms
        // of a 1222ms walk); concurrent, wall = the slowest one.
        let pulled: Vec<Vec<corpus_engine::WikipediaNeighbor>> =
            futures::future::join_all(live_seeds.iter().map(|s| graph.neighbors(s, PPR_SEED_PULL)))
                .await;
        for (i, (s, nbrs)) in live_seeds.iter().zip(pulled).enumerate() {
            for n in &nbrs {
                if typed.len() >= PPR_TYPED_CAP {
                    break;
                }
                if matches!(n.relationship_type.as_str(), "causal" | "contested")
                    && n.in_scope
                    && !already_present.contains(n.title.as_str())
                    && !typed.iter().any(|(t, _, _)| t == &n.title)
                {
                    typed.push((n.title.clone(), s.clone(), n.relationship_type.clone()));
                }
            }
            // Topical bridge channel: the first two seeds' strongest
            // plain links (Hahn from Meitner, Nirvana from Buddhism,
            // Bastille from French Revolution — all topical w=1-2).
            // Reuses the wide pull already in hand; zero extra
            // queries. The prerank separates the wheat.
            if i < 2 {
                let mut took = 0usize;
                for n in &nbrs {
                    if took >= PPR_TOPICAL_PER_SEED || typed.len() >= PPR_TYPED_CAP {
                        break;
                    }
                    if n.relationship_type == "topical"
                        && n.in_scope
                        && !already_present.contains(n.title.as_str())
                        && !typed.iter().any(|(t, _, _)| t == &n.title)
                    {
                        typed.push((n.title.clone(), s.clone(), n.relationship_type.clone()));
                        took += 1;
                    }
                }
            }
            seed_pulls.insert(s.clone(), nbrs);
        }
        sub_pulls_ms = t_sub.elapsed().as_millis() as u64;
        t_sub = std::time::Instant::now();
        for _hop in 0..PPR_HOPS {
            let mut frontier: Vec<(String, f64)> = mass.into_iter().collect();
            frontier.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            frontier.truncate(PPR_FRONTIER_CAP);
            let mut next: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            // Frontier pulls are independent point-queries; fetch the
            // uncached ones concurrently (same rationale as the seed
            // pulls above).
            let fetched: Vec<(String, f64, Vec<corpus_engine::WikipediaNeighbor>)> =
                futures::future::join_all(frontier.into_iter().map(|(title, m)| {
                    let cached = seed_pulls.remove(&title);
                    async move {
                        let nbrs = match cached {
                            Some(pulled) => pulled,
                            None => graph.neighbors(&title, PPR_NEIGHBORS_PER_NODE).await,
                        };
                        (title, m, nbrs)
                    }
                }))
                .await;
            for (_title, m, nbrs) in fetched {
                let push: Vec<_> = nbrs.into_iter().take(PPR_NEIGHBORS_PER_NODE).collect();
                let total_w: f64 = push.iter().map(|n| n.occurrence_count.max(1) as f64).sum();
                if total_w <= 0.0 {
                    continue;
                }
                for n in push {
                    let share = m * PPR_DAMPING * (n.occurrence_count.max(1) as f64) / total_w;
                    *next.entry(n.title.clone()).or_insert(0.0) += share;
                    *rank.entry(n.title.clone()).or_insert(0.0) += share;
                    in_scope.entry(n.title).or_insert(n.in_scope);
                }
            }
            mass = next;
        }

        let mut by_mass: Vec<(String, f64)> = rank
            .iter()
            .filter(|(t, _)| !already_present.contains(t.as_str()))
            .filter(|(t, _)| in_scope.get(*t).copied().unwrap_or(false))
            .map(|(t, m)| (t.clone(), *m))
            .collect();
        by_mass.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        by_mass.truncate(PPR_MASS_INTO_PRERANK);

        // Merge channels (typed first — they carry the structural
        // signal mass cannot), dedupe by title.
        let mut provenance: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        let mut candidates: Vec<(String, f64)> = Vec::new();
        for (t, seed, rel) in typed.iter() {
            let m = rank.get(t).copied().unwrap_or(0.0);
            provenance.insert(t.clone(), (seed.clone(), rel.clone()));
            candidates.push((t.clone(), m));
        }
        for (t, m) in by_mass {
            if !candidates.iter().any(|(c, _)| c == &t) {
                candidates.push((t, m));
            }
        }
        let typed_n = typed.len();
        sub_hops_ms = t_sub.elapsed().as_millis() as u64;
        let walk_ms = t_walk.elapsed().as_millis() as u64;
        tracing::debug!(
            sub_extract_ms,
            sub_gliner_ms,
            sub_record_ms,
            sub_pulls_ms,
            sub_hops_ms,
            "ppr_walk sub-phase timing"
        );
        if candidates.is_empty() {
            return Vec::new();
        }

        // ── Phase 1b: title prerank — semantic selection of which
        // candidates earn a chunk fetch. Bare-title prefills are tiny
        // (~15ms/pair), so judging 24 titles costs less than one
        // wasted FTS fetch. No absolute threshold here — the chunk
        // gate downstream applies the real bar; this only ORDERS.
        let t_prerank = std::time::Instant::now();
        if candidates.len() > PPR_CANDIDATE_ARTICLES {
            let titles: Vec<String> = candidates.iter().map(|(t, _)| t.clone()).collect();
            match (rerank_fn)(message, titles).await {
                Ok(ts) if ts.len() == candidates.len() => {
                    let mut order: Vec<usize> = (0..candidates.len()).collect();
                    order.sort_by(|&a, &b| {
                        ts[b]
                            .partial_cmp(&ts[a])
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let picked: Vec<(String, f64)> = order
                        .into_iter()
                        .take(PPR_CANDIDATE_ARTICLES)
                        .map(|i| candidates[i].clone())
                        .collect();
                    candidates = picked;
                }
                _ => {
                    // Prerank is an optimization, not a gate — fall
                    // back to channel order (typed first, then mass).
                    candidates.truncate(PPR_CANDIDATE_ARTICLES);
                }
            }
        }
        let prerank_ms = t_prerank.elapsed().as_millis() as u64;

        // ── Phase 2: whole-article fetch + lexical within-article
        // pick. `fetch_chunks_by_title` (title-filtered scan — the
        // dominant-source expansion's primitive) pulls the candidate
        // article's chunks; the top PPR_CHUNKS_PER_ARTICLE by
        // question-token overlap go to the gate. Iterations 1-2 used
        // a global FTS query + exact-title filter instead: ~525ms per
        // call AND the surviving chunks were effectively arbitrary
        // (intros), so even correct candidates (Churchill for a Yalta
        // question) reached the gate with a chunk that carried no
        // answer content and were rightly refused. The pick signal is
        // the same substantive-token overlap `reweight_by_query_
        // relevance` uses — cheap, and the CE gate still judges.
        let t_fetch = std::time::Instant::now();
        let engine = &lane.engine;
        let fetch_corpora: Vec<String> = {
            let Ok(installed) = engine.installed_indexes().await else {
                tracing::warn!("ppr_expand: installed_indexes failed — admitting nothing");
                return Vec::new();
            };
            installed
                .into_iter()
                .filter(|i| {
                    enabled_corpora
                        .map(|e| e.iter().any(|c| c == &i.corpus_id))
                        .unwrap_or(true)
                        && corpus_ceiling
                            .map(|e| e.iter().any(|c| c == &i.corpus_id))
                            .unwrap_or(true)
                })
                .map(|i| i.path.to_string_lossy().into_owned())
                .collect()
        };
        let q_tokens = extract_tokens(message, EVIDENCE_TITLE_MIN_TOKEN_LEN);
        let fetches = candidates.iter().map(|(title, _)| {
            let title = title.clone();
            let paths = fetch_corpora.clone();
            let q_tokens = q_tokens.clone();
            async move {
                let mut article: Vec<corpus_engine::ScoredChunk> = Vec::new();
                for path in &paths {
                    let Ok(idx) = engine.open_index(std::path::Path::new(path)).await else {
                        continue;
                    };
                    if let Ok(chunks) = idx
                        .fetch_chunks_by_title(&title, PPR_ARTICLE_FETCH_LIMIT)
                        .await
                    {
                        article.extend(chunks);
                    }
                    if !article.is_empty() {
                        break;
                    }
                }
                // Rank within the article by substantive question-token
                // overlap; take the top few for the gate.
                let overlap = |c: &corpus_engine::ScoredChunk| -> usize {
                    let body = c.content.to_lowercase();
                    q_tokens
                        .iter()
                        .filter(|t| body.contains(t.as_str()))
                        .count()
                };
                article.sort_by_key(|c| std::cmp::Reverse(overlap(c)));
                article.truncate(PPR_CHUNKS_PER_ARTICLE);
                article
            }
        });
        let cand_chunks: Vec<corpus_engine::ScoredChunk> =
            futures::future::join_all(fetches).await.concat();
        let fetch_ms = t_fetch.elapsed().as_millis() as u64;
        if cand_chunks.is_empty() {
            return Vec::new();
        }

        // ── Phase 3: cross-encoder admission gate ───────────────────
        // One batched call scores the candidate chunks. (Probes 1-3
        // also scored a calibration tail of boundary direct hits for a
        // displacement bar; with the absolute CE-yes bar that became
        // pure telemetry, and its pairs were ~25% of the gate's
        // prefill cost — cut 2026-07-17.)
        let t_gate = std::time::Instant::now();
        // Doc-side bridge conditioning: a typed-edge candidate's doc
        // opens with WHY the graph proposed it, so the cross-encoder
        // judges "is this useful given its connection to the seed"
        // rather than the bare "does this answer the question" that
        // rejects answer-side people (Fermi -2.72 / Einstein -1.18
        // under the bare framing). Query side stays untouched — the
        // shared-prefix KV reuse in score_batch depends on it.
        let fmt_doc = |c: &corpus_engine::ScoredChunk| {
            let bridge = c
                .title
                .as_deref()
                .and_then(|t| provenance.get(t))
                .map(|(seed, rel)| format!("[{rel} link from '{seed}'] "))
                .unwrap_or_default();
            match &c.title {
                Some(t) => format!("{bridge}Title: {t}\n\n{}", c.content),
                None => c.content.clone(),
            }
        };
        let docs: Vec<String> = cand_chunks.iter().map(fmt_doc).collect();
        let scores = match (rerank_fn)(message, docs).await {
            Ok(s) if s.len() == cand_chunks.len() => s,
            Ok(s) => {
                tracing::warn!(
                    got = s.len(),
                    expected = cand_chunks.len(),
                    "ppr_expand: rerank length contract violated — admitting nothing"
                );
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!(error = %e, "ppr_expand: rerank failed — admitting nothing");
                return Vec::new();
            }
        };
        let cand_scores: &[f32] = &scores;
        // The admission bar is the model's absolute yes/no floor:
        // logit(yes) > logit(no), i.e. the cross-encoder judges the
        // chunk to ANSWER the query. Iterations 1-3 additionally
        // required beating max(calibration tail) — the displacement
        // test — and admitted ~nothing anywhere (wiki A/B byte-
        // identical to baseline three times): a candidate chunk that
        // must out-score the best marginal direct hit is a bar even
        // genuine answer-side chunks (Einstein's for the Manhattan
        // question) rarely clear. The absolute floor is the variant
        // the S3 title-cosine probes never had access to — a CE-yes
        // chunk carries answer content by definition, which is
        // precisely what yesterday's displaced-facts injections
        // lacked. Tail scores stay in the audit event so the
        // displacement margin remains observable per admission.
        let bar = 0.0_f32;

        let mut admitted_idx: Vec<usize> = (0..cand_chunks.len())
            .filter(|&i| cand_scores[i] > bar)
            .collect();
        admitted_idx.sort_by(|&a, &b| {
            cand_scores[b]
                .partial_cmp(&cand_scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        admitted_idx.truncate(PPR_MAX_ADMITTED);
        let gate_ms = t_gate.elapsed().as_millis() as u64;

        // Admitted chunks carry their gate metadata; PLACEMENT is the
        // join step's job (`place_ppr_admitted`) — it needs the live
        // pool, which this spawned task deliberately never sees.
        let audit: Vec<String> = candidates
            .iter()
            .map(|(t, m)| format!("{t} (mass {m:.4})"))
            .collect();
        let mut added: Vec<corpus_engine::ScoredChunk> = Vec::new();
        for &i in admitted_idx.iter() {
            let mut c = cand_chunks[i].clone();
            c.metadata
                .insert("injected_by".to_string(), "ppr_expand".to_string());
            c.metadata
                .insert("ppr_ce_score".to_string(), format!("{:.4}", cand_scores[i]));
            c.metadata
                .insert("ppr_bar".to_string(), format!("{bar:.4}"));
            added.push(c);
        }

        let scored: Vec<String> = cand_chunks
            .iter()
            .zip(cand_scores.iter())
            .map(|(c, s)| format!("{}:{s:+.2}", c.title.as_deref().unwrap_or("?")))
            .collect();
        eprintln!(
            "[ppr_expand] seeds={live_seeds:?} typed={typed_n} candidates={audit:?} bar={bar:.3} \
             scored={scored:?} admitted={}/{} walk={walk_ms}ms prerank={prerank_ms}ms fetch={fetch_ms}ms gate={gate_ms}ms",
            added.len(),
            cand_scores.len(),
        );
        tracing::info!(
            target: "retrieval_audit",
            event = "ppr_expand",
            query = %truncate_with_ellipsis(message, 120),
            seeds = ?live_seeds,
            typed = typed_n,
            candidates = ?audit,
            cand_scores = ?cand_scores,
            bar,
            admitted = added.len(),
            walk_ms,
            prerank_ms,
            fetch_ms,
            gate_ms,
            "retrieval_audit: ppr_expand"
        );
        added
    }
}

/// Join-time placement for gate-admitted chunks: mid-pool, not
/// boundary. The merged sort orders by `vector_distance` (`None`
/// sorts LAST — a bare injected chunk would be truncated straight
/// out). Boundary placement (probes 1-3b) survived the truncate but
/// died downstream: `expand_from_dominant_source` rebuilds the pool
/// as dominant chunks + the FIRST few non-dominant chunks in pool
/// order, so tail-placed admissions were always its `dropped_noise`.
/// An admitted chunk out-scored the marginal direct hits on
/// cross-encoder judgment — mid-pool standing is what that means
/// operationally. Chunks whose title is already in the live pool are
/// dropped (the pool may have gained them while the lane ran).
pub(crate) fn place_ppr_admitted(
    admitted: Vec<corpus_engine::ScoredChunk>,
    pool: &[corpus_engine::ScoredChunk],
) -> Vec<corpus_engine::ScoredChunk> {
    let present: std::collections::HashSet<&str> =
        pool.iter().filter_map(|c| c.title.as_deref()).collect();
    let boundary = pool.len().min(KQ_MERGED_LIMIT);
    let anchor = pool[..boundary.div_euclid(2).max(1)]
        .iter()
        .rev()
        .find_map(|c| c.vector_distance)
        .unwrap_or(1.0);
    admitted
        .into_iter()
        .filter(|c| {
            !c.title
                .as_deref()
                .map(|t| present.contains(t))
                .unwrap_or(false)
        })
        .enumerate()
        .map(|(rank_pos, mut c)| {
            c.vector_distance = Some(anchor - 1e-4 - (rank_pos as f32) * 1e-5);
            c
        })
        .collect()
}

impl Runtime {
    /// Heuristic question decomposition (opt-in via
    /// `SOVEREIGN_QUERY_DECOMP=1`). Pure-Rust, zero LLM calls.
    ///
    /// Today only handles the comparison shape: a question with ≥2
    /// extractable entities ("Buddhism", "Christianity") and a
    /// shared topic noun ("compassion") gets decomposed into one
    /// sub-query per entity, each pairing the entity with the topic
    /// — `["Buddhism compassion", "Christianity compassion"]`. The
    /// caller fans these out as supplementary retrievals.
    ///
    /// Why heuristic, not LLM: a small fast-slot model (2B-7B)
    /// reliably hallucinates topics absent from the question even
    /// under tight prompts ("Buddhism Christianity differ" → the
    /// model emits "salvation/afterlife" regardless of what the
    /// user actually asked about). Heuristic decomp uses the
    /// literal words from the question, so a topical word the user
    /// typed will appear in every sub-query that mentions it.
    ///
    /// Returns `None` when the gate is off, the question doesn't
    /// match the comparison shape, or no axis term is detectable.
    /// The caller then proceeds with no decomposition.
    pub(crate) fn decompose_question(&self, message: &str, intent: &Intent) -> Option<Vec<String>> {
        if std::env::var("SOVEREIGN_QUERY_DECOMP").ok().as_deref() != Some("1") {
            return None;
        }
        let queries = decompose_question_inner(message, intent)?;
        eprintln!("[query_decomp] queries={queries:?}");
        tracing::info!(
            queries = ?queries,
            "query_decomp: heuristic decomposition"
        );
        Some(queries)
    }
    /// Run each decomposed sub-query through the standard corpus
    /// search and append results to the existing chunk pool. No
    /// score-decay — sub-queries are *also* the user's question,
    /// just decomposed; their hits compete on equal footing with
    /// the bag-of-words original via the downstream reweight step.
    /// Returns the number of chunks added (for logging).
    pub(crate) async fn fan_out_decomposed_queries(
        &self,
        sub_queries: &[String],
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        label: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> usize {
        // Score-decay for fanned-out (sub-query) hits. Default 1.0 keeps
        // the original "compete on equal footing" behaviour. A value <1
        // makes sub-query hits AUGMENT rather than DISPLACE the base
        // query's hits: on an already-focused question (e.g. "the Dynegy
        // transaction") the strong base hits stay atop the window so the
        // answer doesn't diffuse, while a weak-base broad/multi-aspect
        // question still surfaces its decayed-but-present sub-query hits.
        // (Observed: undecayed expansion lifted broad categories +8/+17pt
        // but regressed the focused `deal` question −25pt; decay targets
        // exactly that displacement.) Env-tunable for tight iteration.
        let decay: f32 = std::env::var("SOVEREIGN_DECOMP_DECAY")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|&d| d > 0.0 && d <= 1.0)
            .unwrap_or(1.0);
        let mut added = 0usize;
        for sq in sub_queries {
            let emb = self.inference.embed_query(sq).await.unwrap_or_default();
            let mut hits = self
                .search_corpus_indexes_with_overrides(
                    &emb,
                    sq,
                    DECOMP_QUERY_LIMIT,
                    label,
                    None,
                    enabled_corpora,
                    corpus_ceiling,
                )
                .await;
            if decay < 1.0 {
                for h in &mut hits {
                    h.score *= decay;
                }
            }
            added += hits.len();
            chunks.extend(hits);
        }
        added
    }

    /// The LLM demand planner (I4-A, EPISTEMIC_STATE.md P1b /
    /// RETRIEVAL_REDESIGN S2). ONE Housekeep fast-slot structured-output
    /// call names the turn's retrieval demands: focused sub-queries, the
    /// named entities the answer needs, an optional stance contrast (for
    /// contested questions), and optional section terms. Mirrors
    /// `formulate_evidence_queries`'s fast-slot template — a JSON-schema
    /// structured output (NOT a lark grammar, NOT the forced-choice
    /// sentinel the fast slot doesn't honor), temp 0, ~160-token budget,
    /// and the same nested-JSON sanitize pass. Returns `None` when the
    /// call fails or yields nothing usable (the step then no-ops).
    pub(crate) async fn formulate_demand_plan(
        &self,
        message: &str,
        round0: &[corpus_engine::ScoredChunk],
        context: &ConversationContext,
    ) -> Option<crate::runtime::retrieval_pipeline::DemandPlan> {
        use crate::runtime::retrieval_pipeline::{DemandPlan, StanceContrast};

        // World grounding: source titles seen so far + the enabled corpora,
        // so the planner names entities from THIS knowledge base only.
        let mut titles: Vec<String> = Vec::new();
        for c in round0 {
            if let Some(t) = c.title.as_deref() {
                let t = t.trim();
                if !t.is_empty() && !titles.iter().any(|x| x == t) {
                    titles.push(t.to_string());
                }
            }
            if titles.len() >= 6 {
                break;
            }
        }
        let corpora: Vec<String> = context
            .conversation
            .enabled_corpora
            .clone()
            .unwrap_or_default();
        // Stance groundwork: seed the prompt hint from the deterministic
        // comparison analyzer (comparison_axis / extract_comparison_entities).
        let cmp_entities = crate::runtime::question_analysis::extract_comparison_entities(message);
        let axis_hint = crate::runtime::question_analysis::comparison_axis(message, &cmp_entities);

        let mut world = String::new();
        if !corpora.is_empty() {
            world.push_str(&format!(
                "Knowledge base being searched: {}.\n",
                corpora.join(", ")
            ));
        }
        if !titles.is_empty() {
            world.push_str(&format!(
                "Source documents seen so far: {}.\n",
                titles.join("; ")
            ));
        }
        if let Some(axis) = &axis_hint {
            world.push_str(&format!(
                "The question appears to weigh opposing positions on: {axis}.\n"
            ));
        }
        let prompt = format!(
            "{world}\nPlan the retrieval demands for answering this question FROM THIS \
             KNOWLEDGE BASE ONLY:\n\n{}\n\n\
             Return JSON with: `sub_queries` (up to 5 focused search queries — plain \
             words, names and concrete nouns only, no punctuation); `entities` (the \
             named people, places, things, or events whose details the answer needs); \
             `stance_contrast` (ONLY when the question weighs two opposing positions: \
             the `axis` they disagree on plus exactly two `poles` naming the sides); \
             `section_terms` (document section labels likely to hold the answer, e.g. \
             \"criticism\", \"legacy\" — omit when none is obvious). Stay inside the \
             world named above; never introduce names from other topics.",
            message.chars().take(400).collect::<String>(),
        );
        // SLOT_POLICY §3 Housekeep: schema-constrained planning consumed by
        // the pipeline. Structured output, not forced-choice.
        let mut req =
            CompletionRequest::for_workload(crate::slot_policy::Workload::Housekeep, prompt)
                .with_system("You plan precise retrieval demands.")
                .with_output_budget(160);
        req.temperature = Some(0.0);
        req.enable_thinking = Some(false);
        req.structured_output = Some(serde_json::json!({
            "type": "object",
            "properties": {
                "sub_queries": {
                    "type": "array",
                    "items": { "type": "string", "maxLength": 80 },
                    "maxItems": 5
                },
                "entities": {
                    "type": "array",
                    "items": { "type": "string", "maxLength": 80 },
                    "maxItems": 8
                },
                "stance_contrast": {
                    "type": "object",
                    "properties": {
                        "axis": { "type": "string", "maxLength": 80 },
                        "poles": {
                            "type": "array",
                            "items": { "type": "string", "maxLength": 60 },
                            "minItems": 2, "maxItems": 2
                        }
                    },
                    "required": ["axis", "poles"],
                    "additionalProperties": false
                },
                "section_terms": {
                    "type": "array",
                    "items": { "type": "string", "maxLength": 40 },
                    "maxItems": 5
                }
            },
            "additionalProperties": false
        }));
        let resp = self.inference.complete(&req).await.ok()?;

        #[derive(serde::Deserialize, Default)]
        struct RawStance {
            #[serde(default)]
            axis: String,
            #[serde(default)]
            poles: Vec<String>,
        }
        #[derive(serde::Deserialize, Default)]
        struct Raw {
            #[serde(default)]
            sub_queries: Vec<String>,
            #[serde(default)]
            entities: Vec<String>,
            #[serde(default)]
            stance_contrast: Option<RawStance>,
            #[serde(default)]
            section_terms: Vec<String>,
        }
        let text = resp.text.trim();
        let raw: Raw = serde_json::from_str(text)
            .or_else(|_| {
                let start = text.find('{').unwrap_or(0);
                serde_json::from_str(&text[start..])
            })
            .ok()?;

        // Same sanitize pass as formulate_evidence_queries: the fast model
        // sometimes nests JSON-looking text inside string items (the schema
        // constrains the envelope, not the contents). Strip structural
        // characters, collapse whitespace, drop empties/over-long items.
        let sanitize = |v: Vec<String>, max: usize| -> Vec<String> {
            v.into_iter()
                .map(|q| {
                    q.chars()
                        .map(|c| if "{}\"\\:".contains(c) { ' ' } else { c })
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|q| !q.is_empty() && q.len() <= max)
                .collect()
        };
        let mut sub_queries = sanitize(raw.sub_queries, 80);
        sub_queries.truncate(5);
        let mut entities = sanitize(raw.entities, 80);
        entities.truncate(8);
        let mut section_terms = sanitize(raw.section_terms, 40);
        section_terms.truncate(5);
        let stance_contrast = raw.stance_contrast.and_then(|rs| {
            let axis = rs.axis.trim().to_string();
            let poles = sanitize(rs.poles, 60);
            // A stance needs a named axis AND exactly two poles, else drop it.
            if axis.is_empty() || poles.len() != 2 {
                None
            } else {
                Some(StanceContrast { axis, poles })
            }
        });

        if sub_queries.is_empty()
            && entities.is_empty()
            && stance_contrast.is_none()
            && section_terms.is_empty()
        {
            return None;
        }
        Some(DemandPlan {
            sub_queries,
            entities,
            stance_contrast,
            section_terms,
        })
    }

    /// Abstract-question → concrete article-title expansion.
    ///
    /// Targets the failure mode `decompose_question` doesn't reach:
    /// questions with zero extractable entities, where the answer
    /// lives in a Wikipedia article whose title is a single concrete
    /// noun the question never says. Marathon (v16 audit) examples:
    ///
    /// - T4 "How did computing develop from there in the next
    ///   century?" — bench expects 20th-century / electronic /
    ///   WWII content. Question lacks era keywords; embedding lands
    ///   on SEP `computing-history` (pre-electronic narrative).
    /// - T7 "After the war, what architecture became standard for the
    ///   first electronic computers?" — bench expects
    ///   `Von Neumann architecture` + `ENIAC` Wikipedia articles.
    ///   Question never names them; one VN-arch chunk at position 14.
    /// - T9 "What hardware breakthrough in the 1950s made smaller
    ///   computers possible?" — bench expects `Transistor`. Zero
    ///   transistor chunks retrieved; model hallucinated
    ///   `ferrite core memory`.
    ///
    /// Approach: one Fast-slot LLM call with a JSON-schema-
    /// constrained output of 2-3 Wikipedia titles. The titles are
    /// fanned out via the existing `fan_out_decomposed_queries`
    /// helper so the new path reuses the same search-and-merge
    /// shape as the comparison decomposer.
    ///
    /// Opt-in via `SOVEREIGN_TITLE_EXPAND=1`. Default off — the
    /// primitive adds ~400-800ms per turn (one LLM call + N embed
    /// + N searches), and we want bench-level evidence that the
    /// added latency buys the recall lift before turning it on
    /// for all KnowledgeQuery traffic.
    ///
    /// Returns `None` when the gate is off, the LLM call fails,
    /// the JSON doesn't parse, or no titles emerge. Caller
    /// proceeds without expansion in any of those cases.
    pub(crate) async fn expand_question_to_titles(
        &self,
        message: &str,
        context: &ConversationContext,
    ) -> Option<Vec<String>> {
        if std::env::var("SOVEREIGN_TITLE_EXPAND").ok().as_deref() != Some("1") {
            return None;
        }

        // Build a short conversation-context block — the LLM
        // benefits from knowing what the user has been asking
        // about so "from there" / "after the war" can resolve to
        // the right era. We bound the included history at 4
        // messages to keep the prompt tight and the Fast-slot
        // call fast.
        let recent: Vec<&Message> = context
            .conversation
            .messages
            .iter()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let recent_summary: String = recent
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "User",
                    Role::Assistant => "Assistant",
                    Role::System => "System",
                };
                let mut end = m.content.len().min(200);
                while end > 0 && !m.content.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{role}: {}", &m.content[..end])
            })
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Given the conversation and the user's current question, list 2-4 \
             focused search queries that would retrieve the source documents \
             answering it. Each query should name ONE specific entity, person, \
             organization, concept, or subtopic the answer involves — concrete \
             terms likely to appear verbatim in the documents, NOT a paraphrase \
             of the broad question. For a multi-part question (\"X, Y, or Z\"), \
             give a separate query per part so each aspect is retrieved on its \
             own rather than averaged into one muddy query. Corpus-agnostic: \
             these may be Wikipedia-style titles (\"Von Neumann architecture\") \
             or corpus-specific names (\"Dynegy merger\", \"mark-to-market \
             accounting\"). If the question pivots to a new topic, target the \
             new topic.\n\n\
             Recent conversation:\n{recent_summary}\n\n\
             Current question: {message}\n\n\
             Reply with JSON only:\n\
             {{\"titles\": [\"query 1\", \"query 2\"]}}"
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "titles": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["titles"]
        });

        let request = CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(120),
            temperature: Some(0.1),
            think_budget: Some(0),
            structured_output: Some(schema),
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
            prompt_shape: None,
            stable_prefix_len: None,
            ..Default::default()
        };

        let response = match self.inference.complete(&request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "title_expand: Fast-slot LLM call failed; skipping expansion"
                );
                return None;
            }
        };
        let raw = response.text.trim();
        let json_str = raw
            .strip_prefix("```json")
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(raw)
            .trim();
        let parsed: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    raw = %raw,
                    "title_expand: parse failed; skipping expansion"
                );
                return None;
            }
        };
        let titles: Vec<String> = parsed
            .get("titles")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .take(3)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if titles.is_empty() {
            return None;
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "title_expand",
            query = %truncate_with_ellipsis(message, 120),
            titles = ?titles,
            "retrieval_audit: title_expand"
        );
        Some(titles)
    }
}

/// Pure heuristic question decomposition — the body of
/// [`Runtime::decompose_question`] WITHOUT the `SOVEREIGN_QUERY_DECOMP`
/// env gate. Split out (2026-07-18) so the epistemic demand builder can
/// derive sub-question facets from the SAME decomposition without
/// enabling retrieval fan-out — the env gate stays on the fan-out
/// caller only (EPISTEMIC_STATE.md, Milestone B).
///
/// Comparison shape only: ≥2 extractable entities plus a detectable
/// axis term → one `"{entity} {axis}"` sub-query per entity. `None`
/// when the shape doesn't match.
pub(crate) fn decompose_question_inner(message: &str, intent: &Intent) -> Option<Vec<String>> {
    // Only fire on shapes where per-entity decomposition is
    // structurally meaningful. KnowledgeQuery is included because the
    // classifier sometimes routes "How do X and Y differ on Z?" to
    // KnowledgeQuery rather than ComparisonQuery.
    if !matches!(
        intent,
        Intent::ComparisonQuery | Intent::KnowledgeQuery | Intent::DeepQuery
    ) {
        return None;
    }

    let entities = extract_comparison_entities(message);
    if entities.len() < 2 {
        return None;
    }

    let axis = comparison_axis(message, &entities)?;

    let queries: Vec<String> = entities
        .iter()
        .take(DECOMP_MAX_QUERIES)
        .map(|e| format!("{e} {axis}"))
        .collect();
    if queries.is_empty() {
        return None;
    }
    Some(queries)
}
