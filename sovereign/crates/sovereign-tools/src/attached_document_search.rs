//! `AttachedDocumentSearchTool` — first-class Tool wrapping the
//! existing RAG retrieval against a document attached to the current
//! conversation.
//!
//! # Why this exists
//!
//! Pre-2026-05-20 the only way to query an attached document was
//! `DocumentAssetManager::ask()`, which ran its own skeleton-based
//! router and either dispatched to a document operation OR signalled
//! `OffTopic` so the caller could fall back to `runtime.handle_turn`.
//! That made the document a *parallel routing universe*: the model
//! under test couldn't choose to call into the document, couldn't
//! chain document lookups with corpus searches, couldn't recover
//! from a thin retrieval via gap-check.
//!
//! The book-report bench (2026-05-20) surfaced the cost: Tier-1
//! factual questions about the attached novel got mis-routed as
//! OffTopic and answered from the wrong sources entirely; Tier-4
//! synthesis hit 100% mechanical / 0/5 judge with fabricated section
//! numbers because the model got one map-reduce shot with no
//! iterative retrieval.
//!
//! # What this tool changes
//!
//! Registering this in `chat_cmd::bootstrap` puts attached-document
//! search into the standard tool catalog. The runtime's
//! `ReasonWithTools` loop can now pick it (alongside
//! `knowledge_search`, `web_fetch`, `write_note`, etc.) when the
//! conversation has a document attached. The existing primitives —
//! tool-call narration chips, `ToolRegistry::with_cache` idempotency,
//! gap-check, graceful-failure prompt rule — all compose without
//! further plumbing.
//!
//! See sovereign decision note `7693f16b` for the broader direction.

use std::sync::Arc;

use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::{InferenceProvider, StateStore, Tool};
use sovereign_core::types::{
    AssetState, Effect, Idempotency, Latency, Permission, RaptorNode,
    Scope, StepOutput, ToolContext, ToolDescriptor,
};

pub struct AttachedDocumentSearchTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl AttachedDocumentSearchTool {
    pub fn new(
        store: Arc<dyn StateStore>,
        inference: Arc<dyn InferenceProvider>,
    ) -> Self {
        Self { store, inference }
    }
}

#[async_trait]
impl Tool for AttachedDocumentSearchTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "attached_doc_search".to_string(),
            name: "Search attached document".to_string(),
            description:
                "Retrieve passages from the document the user has attached to this conversation. \
                 Use this when the question is about the specific text the user shared (a book, \
                 paper, report, etc.). Returns relevant excerpts with their location in the \
                 document. Call repeatedly with different queries to triangulate across the \
                 document; the corpus knowledge tool covers everything outside the attachment."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "A precise natural-language query against the attached \
                                        document. Examples: 'where Stevie's address label is \
                                        described', 'Vladimir's speech about the Greenwich \
                                        Observatory', 'every passage where Winnie is called \
                                        incurious'."
                    }
                },
                "required": ["query"]
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Concatenated passages with inline source labels. Empty / no-doc \
                                response on conversations without an attached document."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![] // Reading the user's own attached document needs no gate.
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        match params.get("query").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => Ok(()),
            _ => Err(Error::InvalidInput(
                "attached_doc_search requires a non-empty `query` string".to_string(),
            )),
        }
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing `query`".to_string()))?;

        // ── Resolve the attached asset ───────────────────────────
        //
        // V1 stub: pick the most-recently-ingested Ready document
        // asset. This works for single-user single-doc cases (the
        // book-report bench, the desktop user with one attachment
        // open). For multi-asset / multi-conversation cases the
        // right shape is to extend `DocumentSession` with an
        // `asset_id: Option<String>` field (or thread asset_id
        // through `ToolContext`) so the tool can resolve by
        // conversation deterministically. Both options are
        // additive — captured as TODO on sovereign decision
        // 7693f16b. The bench's reuse-asset path produces exactly
        // one Ready asset at a time, so the stub is correct in
        // practice while we keep moving.
        //
        // `ctx.conversation_id` is intentionally unused in the
        // stub but kept in scope to signal the right field for
        // future plumbing.
        let _ = &ctx.conversation_id;
        let assets = self.store.list_document_assets().await?;
        let asset = match assets
            .into_iter()
            .filter(|a| matches!(a.state, AssetState::Ready))
            .max_by_key(|a| a.ingested_at)
        {
            Some(a) => a,
            None => {
                return Ok(StepOutput::Text(
                    "No Ready document is attached to this conversation.".to_string(),
                ));
            }
        };

        // ── Raw chunk retrieval ─────────────────────────────────
        //
        // Earlier versions of this tool delegated to
        // `DocumentAssetManager::execute_operation` with
        // `DocumentAssetOperation::Rag`. That path retrieves chunks
        // *and then runs LLM synthesis over them*, returning a
        // finished answer plus snippet citations. When called inside
        // the runtime's ReasonWithTools loop (where the upstream
        // model is supposed to do the synthesis), the result was
        // double-stacked synthesis: an inner LLM call producing
        // "Based on the provided passages, there is no specific
        // identification evidence mentioned…" that the upstream
        // model then synthesised *on top of*, hiding the raw chunks.
        //
        // For a Tool, the right contract is raw evidence: retrieve
        // matching chunks, return them with stable [Source: chunk N]
        // labels, and let the upstream reasoning loop synthesise
        // once. One inference call instead of two; the upstream
        // model sees the document's actual phrasing instead of an
        // inner model's paraphrase.
        let query_embedding = self.inference.embed(query).await?;
        let asset_source_key = asset.source_key();

        // ── Source-filtered K-NN ───────────────────────────────
        //
        // The earlier implementation called
        // `store.search_documents(&query_embedding, query, K)` which
        // ranks across ALL ingested document chunks, then post-
        // filtered down to this asset's chunks. That looks fine in
        // isolation, but two of the bench iterations had the same
        // source text ingested as separate assets (the standing
        // working asset + a leftover from a failed re-ingest);
        // identical Conrad chunks in both produced near-identical
        // embeddings, so the top-K landed exactly 50/50 across
        // assets — wasting half of every K-budget on chunks the
        // tool is contractually about to filter out.
        //
        // Empirically (book-report v1.1 retrieval probe, 2026-05-21):
        //   - top-16 had 8 active + 8 orphan chunks → model saw 8
        //   - the load-bearing `stevie_circles` chunk for the user
        //     question was at rank 2 of *this asset's* chunks but
        //     at rank 4 of the mixed pool — easily knocked out by
        //     orphan ties
        //
        // Fix: pull all chunks for this asset's source_key directly,
        // compute cosine to the query embedding client-side, sort
        // and take top-K. No corpus-engine API change needed and no
        // mutation of stored data. Cost per query: ~316 dot-products
        // (typical attached doc) × embedding-dim, negligible next to
        // the inference round-trip we're inside anyway.
        let mut asset_chunks = self
            .store
            .get_chunks_by_source(&asset_source_key)
            .await
            .unwrap_or_default();
        // Discard chunks without embeddings (shouldn't happen in
        // practice — ingest stores embedding alongside content —
        // but defensive: a missing embedding can't be scored).
        asset_chunks.retain(|c| c.embedding.is_some());
        // Index by chunk_index so we can look up neighbours below
        // without a second store fetch. Same `asset_chunks` is used
        // for cosine scoring AND neighbour rendering — single source
        // of truth.
        let chunk_by_index: std::collections::HashMap<usize, sovereign_core::types::DocumentChunk> =
            asset_chunks
                .iter()
                .cloned()
                .map(|c| (c.chunk_index, c))
                .collect();
        let mut scored: Vec<(f32, usize)> = asset_chunks
            .iter()
            .map(|c| {
                let emb = c.embedding.as_ref().unwrap();
                let score = cosine_similarity(&query_embedding, emb);
                (score, c.chunk_index)
            })
            .collect();

        // Sort by cosine first — this is the load-bearing ranking
        // for narrow factual questions. PPR may add additional
        // chunks below, but it must NOT displace cosine's top hits.
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // ── PPR recall-boost (HippoRAG-style multi-hop) ─────────
        //
        // Bench run #6 (PPR-as-re-rank @ 30%, 2026-05-22) lifted
        // T3 stevie_circles from 0.0 → 4.0 judge by surfacing the
        // entity-walk-discovered chunk, but collapsed T1 winnie_fate
        // from 100% → 20% — PPR re-ranking pushed cosine's load-
        // bearing chunk 957 (Ossipon-reads-newspaper, an epilogue
        // chunk with low central-entity density) out of the top-K.
        //
        // The HippoRAG-faithful pattern is PPR-as-recall-boost,
        // not re-rank: keep cosine's top-K untouched, then ADD
        // high-PPR chunks beyond as additional candidates. The
        // multi-hop signal arrives as breadth, not as displacement.
        // Cosine continues to win factual questions; PPR composes
        // for synthesis questions that need entity-walk reasoning.
        //
        // Invisible to the model — the briefing never surfaces the
        // graph. The only observable effect is additional chunks in
        // the tool result that pure cosine would have missed.
        //
        // Skipped when (a) the skeleton has no action atoms,
        // (b) the query mentions no known entities, or (c) the
        // operator disables it via SOVEREIGN_DOC_PPR=off.
        let ppr_enabled =
            std::env::var("SOVEREIGN_DOC_PPR").map(|v| v != "off").unwrap_or(true);
        let mut ppr_boosted_chunks: Vec<usize> = Vec::new();
        if ppr_enabled {
            if let Some(skel) = asset.skeleton.as_ref() {
                if !skel.actions.is_empty() && !skel.entity_index.is_empty() {
                    let graph = crate::entity_graph::EntityGraph::build(skel);
                    let seeds = graph.seeds_from_query(query);
                    if !seeds.is_empty() {
                        let ppr = graph.personalized_pagerank(
                            &seeds,
                            crate::entity_graph::DEFAULT_DAMPING,
                            crate::entity_graph::DEFAULT_MAX_ITERS,
                            crate::entity_graph::DEFAULT_EPSILON,
                        );
                        let max_chunk_id = scored
                            .iter()
                            .map(|(_, i)| *i)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1);
                        let chunk_ppr = graph.score_chunks(&ppr, max_chunk_id);

                        // Identify the cosine top-K so PPR adds chunks
                        // *beyond* it. Boost count configurable via env
                        // var (default 6); cap of 12 keeps the tool
                        // result bounded even on noisy graphs.
                        let cosine_top_k: usize = 16;
                        let cosine_top_indices: std::collections::HashSet<usize> = scored
                            .iter()
                            .take(cosine_top_k)
                            .map(|(_, i)| *i)
                            .collect();
                        let boost_count: usize = std::env::var("SOVEREIGN_DOC_PPR_BOOST")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .map(|v: usize| v.clamp(0, 12))
                            .unwrap_or(6);

                        // Pick the highest-PPR chunks NOT already in
                        // the cosine top-K. Tie-break by chunk index
                        // for stability (lower index first).
                        let mut candidates: Vec<(usize, f32)> = chunk_ppr
                            .iter()
                            .enumerate()
                            .filter(|(idx, score)| {
                                **score > f32::EPSILON
                                    && !cosine_top_indices.contains(idx)
                                    && chunk_by_index.contains_key(idx)
                            })
                            .map(|(idx, score)| (idx, *score))
                            .collect();
                        candidates.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                                .then(a.0.cmp(&b.0))
                        });
                        ppr_boosted_chunks = candidates
                            .into_iter()
                            .take(boost_count)
                            .map(|(idx, _)| idx)
                            .collect();

                        tracing::debug!(
                            seeds = seeds.len(),
                            entity_count = graph.entity_count(),
                            boosted = ppr_boosted_chunks.len(),
                            "attached_doc_search: PPR recall-boost applied"
                        );
                    }
                }
            }
        }
        // ── RAPTOR cluster-score blend ──────────────────────────
        //
        // Cosine alone tells us which chunks resemble the query.
        // It tells us nothing about which structural NEIGHBOURHOOD
        // those chunks belong to. RAPTOR's leaf clusters do — each
        // chunk lives in a leaf cluster whose `summary_embedding`
        // captures what the surrounding scene is about. Blending a
        // cluster-relevance signal back into the per-chunk score
        // gives the retrieval a soft "the answer is in the document's
        // ending neighbourhood" prior that cosine cannot represent.
        //
        // Inspired by the SEP rerank experiment's `atlas_weight`
        // finding (sovereign/docs/RERANK_EXPERIMENT.md): a structural
        // blend term lifted SEP sources 40→65 of 66 on the canonical
        // bench. The book-report bench's T1 winnie_fate volatility
        // (chunk 957, the Ossipon-reads-newspaper epilogue) and T3
        // motif-recurrence misses share the same shape — cosine
        // bouncing between equally-similar chunks because the model
        // had no neighbourhood signal to break the tie. See spec at
        // `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`.
        //
        // Default OFF (cluster_weight = 0.0) — byte-identical
        // baseline. Operators / bench runs flip it on via env var.
        let cluster_weight = std::env::var("SOVEREIGN_DOC_CLUSTER_WEIGHT")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .map(|w| w.clamp(0.0, 1.0))
            .unwrap_or(0.0);
        if cluster_weight > 0.0 {
            // Pool size widens the slice of cosine candidates we
            // re-rank. The rerank experiment showed wider pools are
            // what let structural blends earn their keep — at the
            // default 16 there's no headroom for the cluster signal
            // to promote chunks above the cosine top-1/2 even when
            // they're in the right neighbourhood.
            let cluster_pool_size: usize = std::env::var("SOVEREIGN_DOC_CLUSTER_POOL")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(|n: usize| n.clamp(1, 256))
                .unwrap_or(16);
            // Empty at PartiallyReady / MultiHopReady — T3 hasn't
            // landed yet. `blend_by_cluster_score` handles empty
            // gracefully (falls through to cosine ordering).
            let leaf_nodes: Vec<RaptorNode> = self
                .store
                .list_raptor_nodes(&asset.id)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|n| n.level == 0)
                .collect();
            // Build the candidate pool from BOTH cosine top-K and
            // PPR-boosted chunks. PPR's contribution stays as recall
            // (adding chunks beyond cosine top-K), but those chunks
            // are still legitimate candidates for the blended
            // ranking — they earn a slot when their cluster is the
            // right neighbourhood.
            let mut pool: Vec<(usize, f32)> = scored
                .iter()
                .take(cluster_pool_size)
                .map(|(s, i)| (*i, *s))
                .collect();
            let in_pool: std::collections::HashSet<usize> =
                pool.iter().map(|(i, _)| *i).collect();
            // PPR chunks may sit below the cosine top-K cut. Look
            // their cosine score up from `scored` (the full sorted
            // list). Cosine score is unique per chunk, so a linear
            // scan over `scored` is fine — `ppr_boosted_chunks` is
            // capped at 12.
            for &ppr_idx in &ppr_boosted_chunks {
                if in_pool.contains(&ppr_idx) {
                    continue;
                }
                if let Some((s, _)) = scored.iter().find(|(_, i)| *i == ppr_idx) {
                    pool.push((ppr_idx, *s));
                }
            }
            let ranked = blend_by_cluster_score(
                &pool,
                &leaf_nodes,
                &query_embedding,
                cluster_weight,
            );
            // Map the new chunk-index ordering back into the
            // (cosine_score, chunk_index) shape downstream code
            // expects. Cosine scores carry through unchanged — they
            // aren't read after this point, but keeping them avoids
            // a wider refactor of `scored`'s type. The blended order
            // is what matters.
            let cosine_by_chunk: std::collections::HashMap<usize, f32> = pool
                .iter()
                .copied()
                .collect();
            scored = ranked
                .into_iter()
                .map(|i| (*cosine_by_chunk.get(&i).unwrap_or(&0.0), i))
                .collect();
            tracing::debug!(
                cluster_weight,
                cluster_pool_size,
                leaf_nodes = leaf_nodes.len(),
                pool_size = pool.len(),
                "attached_doc_search: cluster-score blend applied"
            );
        }

        // K=16. Per-turn latency probes 2026-05-21 measured the
        // accumulating-prefill cost as the dominant per-iteration
        // wall-clock — 25s → 44s between iterations as the
        // conversation doubled. Tried K=12 to halve tool result
        // size; gave ~19% per-turn latency saving on stevie_circles
        // probe but full-20 bench showed T4 judge dropped 4.0→3.0
        // and T5 judge collapsed 1.3→0.0. The model uses retrieval
        // BREADTH (chunks 13-16) for cross-scene synthesis even
        // though it rarely CITES them directly — they shape the
        // context the synthesis prompt sees.
        //
        // Future perf paths that won't hurt synthesis: shrink the
        // briefing (resent on every iteration's prefill); drop old
        // tool results once newer queries on the same topic
        // supersede them; use a draft model for tool-decision
        // turns (Primary for final synthesis only).
        scored.truncate(16);

        // ── Mechanical ±1 chunk-neighbour expansion ─────────────
        //
        // We experimented (2026-05-21) with using LLM-judged
        // `DocumentSegment`s as the expansion unit — lift each
        // cosine hit to its containing segment so the model sees
        // a structurally coherent block. Single-question probe
        // on T3/T4 synthesis lifted as predicted (T3 judge
        // 3.4→4.4), but the full 20-question bench showed the
        // trade was net-negative: T1 mech regressed 18 pts from
        // the K-and-chunk-cap squeeze needed to keep prompts
        // bounded when segments could be 70+ chunks; T5 judge
        // collapsed (2.5→0.0). Aggregate 72%→69% mech, 3.69→3.47
        // judge.
        //
        // Mechanical ±1 won that bake-off. Segments are still
        // built at ingest and surfaced in the briefing as a
        // scene map (so the model can use scene titles to
        // formulate queries) but they no longer drive retrieval.
        // See handoff doc for the architectural lesson:
        // LLM-judged scenes are good *labels*, not necessarily
        // good *retrieval units*.
        //
        // 2026-05-22 update: ±1 itself is now disabled by default
        // — see the SOVEREIGN_DOC_CHUNK_NEIGHBOURS gate below for
        // the bench data and rationale.
        let hit_indices: std::collections::HashSet<usize> =
            scored.iter().map(|(_, i)| *i).collect();
        // PPR-boosted chunks join the hit set as primary hits — they
        // get ±1 neighbour expansion same as cosine hits because the
        // model needs surrounding context to reason about whatever
        // the entity-walk surfaced. They're flagged separately below
        // when rendering so the model can see WHY they were retrieved
        // (entity walk vs cosine similarity), but they count as HITs.
        let ppr_hit_set: std::collections::HashSet<usize> =
            ppr_boosted_chunks.iter().copied().collect();
        let combined_hits: std::collections::HashSet<usize> = hit_indices
            .iter()
            .chain(ppr_hit_set.iter())
            .copied()
            .collect();
        // ±1 expansion roughly triples tool-result size (each hit
        // grows to a 3-chunk window). On the diagnostic triplet, a
        // 4-rep A/B (2026-05-22) measured ±1 ON at 611s wall vs ±1
        // OFF mean 340s across 4 reps (range 305-372) — a robust
        // −44% / −271s win, far outside this bench's variance band.
        // Quality changes were within variance: T1 flat (20% in all
        // 4 reps and baseline), T3 mech 80→70 mean (still 60 or 80
        // each rep), T5 judge actually slightly higher on the OFF
        // mean. The handoff's previous quality-lift claim (T3 judge
        // +1.6, T5 judge +1.2) was measured pre-RAPTOR-atlas; the
        // atlas's scene-map signposts in the briefing plausibly
        // absorb what ±1 used to buy, leaving the cost without the
        // contribution. Default flipped to OFF 2026-05-22; operators
        // wanting the previous behaviour set SOVEREIGN_DOC_CHUNK_NEIGHBOURS=1.
        let expand_neighbours = std::env::var("SOVEREIGN_DOC_CHUNK_NEIGHBOURS")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .map(|n| n != 0)
            .unwrap_or(false);
        let expanded_ordered: Vec<usize> = {
            let mut expanded: std::collections::BTreeSet<usize> =
                combined_hits.iter().copied().collect();
            if expand_neighbours {
                for &h in &combined_hits {
                    if h > 0 {
                        expanded.insert(h - 1);
                    }
                    expanded.insert(h + 1);
                }
            }
            expanded
                .into_iter()
                .filter(|i| chunk_by_index.contains_key(i))
                .collect()
        };

        let relevant_owned: Vec<sovereign_core::types::DocumentChunk> = expanded_ordered
            .iter()
            .filter_map(|i| chunk_by_index.get(i).cloned())
            .collect();
        let relevant: Vec<&sovereign_core::types::DocumentChunk> = relevant_owned.iter().collect();
        // `raw` retained as a name below so the atom-anchored block's
        // chunk lookup keeps working.
        let raw: Vec<sovereign_core::types::DocumentChunk> = relevant_owned.clone();

        // ── Atom-anchored retrieval (atlas-light) ───────────────
        //
        // Before formatting, check whether any of the document's
        // ranked entities appear in the query. If so, look up
        // action atoms for those entities and union the atom-
        // referenced chunks into the result set. This bridges the
        // semantic-similarity gap RAG alone can't cross — when the
        // chunk holding "Winnie stitched the address label into
        // the lapel" doesn't embed close to "address bomber
        // identification" queries, the atom index still points to
        // it directly via the entity name.
        let query_lower = query.to_lowercase();
        let mut atom_chunks: Vec<(usize, String, String)> = Vec::new(); // (chunk_index, atom_summary, evidence)
        let mut atom_chunk_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        if let Some(skeleton) = asset.skeleton.as_ref() {
            for ent in &skeleton.main_entities {
                let ent_lower = ent.name.to_lowercase();
                // Loose match — the model might query "Winnie" while
                // the canonical name is "Winnie Verloc"; substring
                // either way is fine.
                let query_mentions_entity = query_lower.contains(&ent_lower)
                    || ent_lower
                        .split_whitespace()
                        .any(|tok| tok.len() > 3 && query_lower.contains(tok));
                if !query_mentions_entity {
                    continue;
                }
                for atom in skeleton.actions.iter().filter(|a| a.entity == ent.name) {
                    if atom_chunk_indices.insert(atom.chunk_index) {
                        atom_chunks.push((
                            atom.chunk_index,
                            format!("{} {} {}", atom.entity, atom.verb, atom.object),
                            atom.evidence.clone(),
                        ));
                    }
                }
            }
        }

        if relevant.is_empty() && atom_chunks.is_empty() {
            return Ok(StepOutput::Text(format!(
                "The attached document ({}) was searched for \"{}\" but no relevant \
                 passages were found.",
                asset.title, query
            )));
        }

        // Format: group consecutive chunks into windows, marking
        // which ones were the actual cosine hits vs which are
        // contextual neighbours. The model is told to cite hits as
        // load-bearing evidence and use context for synthesis.
        //
        // Recall-boost chunks (from PPR entity-walk discovery) are
        // tagged distinctly as "HIT (entity-walk)" so the model can
        // see HOW each chunk was surfaced — useful for the model's
        // reasoning trace and for downstream diagnostics. They count
        // as HITs (load-bearing evidence), just discovered via the
        // graph instead of cosine.
        let hit_count = hit_indices.len() + ppr_hit_set.len();
        let mut formatted = format!(
            "Retrieved {} passage(s) from \"{}\" with surrounding context ({} chunks total):\n\n",
            hit_count + atom_chunks.len(),
            asset.title,
            relevant.len(),
        );
        // Walk `relevant` (document-ordered). Open a new window
        // whenever there's a gap in chunk_index, close the previous
        // one. Inside a window, each chunk is tagged HIT or context.
        let mut prev_idx: Option<usize> = None;
        for c in &relevant {
            // Skip chunks we'll also emit via atoms — avoid duplicate
            // blocks for the upstream model.
            if atom_chunk_indices.contains(&c.chunk_index) {
                prev_idx = Some(c.chunk_index);
                continue;
            }
            let is_new_window = match prev_idx {
                Some(p) => c.chunk_index > p + 1,
                None => true,
            };
            if is_new_window {
                if prev_idx.is_some() {
                    formatted.push('\n');
                }
                formatted.push_str(&format!(
                    "── Window starting at chunk {} ──\n",
                    c.chunk_index,
                ));
            }
            let snippet_chars: String = c.content.chars().take(600).collect();
            let suffix = if c.content.chars().count() > 600 {
                "…"
            } else {
                ""
            };
            // Check entity-walk origin first so a PPR chunk that
            // also survived the cluster-blend re-ranking keeps the
            // "(entity-walk)" tag — the tag reflects HOW retrieval
            // surfaced the chunk, not WHERE it ranked. Pre-blend
            // (cluster_weight=0.0) the two sets are disjoint by
            // construction so the order is behaviour-neutral.
            let tag = if ppr_hit_set.contains(&c.chunk_index) {
                "HIT (entity-walk)"
            } else if hit_indices.contains(&c.chunk_index) {
                "HIT"
            } else {
                "context"
            };
            formatted.push_str(&format!(
                "[Source: chunk {} | {}] {}{}\n",
                c.chunk_index,
                tag,
                snippet_chars.trim(),
                suffix,
            ));
            prev_idx = Some(c.chunk_index);
        }
        formatted.push('\n');
        // Then atom-anchored chunks, each tagged with the action
        // atom that surfaced it. Look up full chunk content from the
        // store so the model sees the passage, not just the evidence
        // snippet (which is capped at 140 chars).
        for (chunk_idx, atom_summary, atom_evidence) in &atom_chunks {
            // Find the chunk content. We didn't request it by index
            // above, so look it up in `raw` (top-K results from the
            // embedding query, which may or may not include it) and
            // fall back to a list_documents call if necessary.
            let chunk_content_opt = raw.iter().find(|c| {
                c.source == asset_source_key && c.chunk_index == *chunk_idx
            });
            let body: String = match chunk_content_opt {
                Some(c) => c.content.chars().take(600).collect(),
                None => {
                    // Hit the store directly for the chunk content.
                    // `get_chunks_by_source` returns every chunk for
                    // this asset; that's heavier than needed but
                    // unavoidable without an index-by-(source, idx)
                    // lookup. Cached after first call within the
                    // tool execution.
                    let all = self
                        .store
                        .get_chunks_by_source(&asset_source_key)
                        .await
                        .unwrap_or_default();
                    all.into_iter()
                        .find(|c| c.chunk_index == *chunk_idx)
                        .map(|c| c.content.chars().take(600).collect())
                        .unwrap_or_else(|| atom_evidence.clone())
                }
            };
            formatted.push_str(&format!(
                "[Source: chunk {} | atom: {}] {}\n\n",
                chunk_idx,
                atom_summary,
                body.trim(),
            ));
        }
        Ok(StepOutput::Text(formatted))
    }

    fn retry_config(&self) -> Option<sovereign_core::types::RetryConfig> {
        // Retrieval is deterministic; no retry. Idempotency handles
        // duplicate calls via the registry's cache.
        None
    }
}

/// Cosine similarity between two equal-length f32 vectors. Returns
/// 0.0 when either norm is zero (handles defensive empty/null embeds
/// without panicking). Pure function; no allocations.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Min-max normalise a slice into `[0, 1]`. When all values are
/// equal (within `f32::EPSILON`), returns a constant `0.5` for every
/// element — a neutral midpoint that lets the score contribute
/// nothing to a blend rather than poisoning the result with NaN or
/// collapsing every chunk to zero.
fn min_max_normalize(xs: &[f32]) -> Vec<f32> {
    if xs.is_empty() {
        return Vec::new();
    }
    let max = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let min = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let range = max - min;
    if range <= f32::EPSILON {
        return vec![0.5; xs.len()];
    }
    xs.iter().map(|x| (x - min) / range).collect()
}

/// Re-rank a candidate pool by blending cosine relevance with the
/// query's relevance to each chunk's RAPTOR leaf-cluster summary.
///
/// `pool` is the (chunk_index, cosine_score) candidates to consider.
/// Order does not matter on input; the returned `Vec<usize>` is
/// the chunk indices sorted by blended score, descending.
///
/// `leaf_nodes` MUST be RAPTOR level-0 nodes (caller filters). When
/// empty — e.g. the asset hasn't reached T3 yet, or the atlas builder
/// hasn't run — the function returns the pool's cosine ordering. The
/// caller sees a graceful baseline instead of a panic.
///
/// `weight` is the cluster blend weight in `[0, 1]`. The caller is
/// expected to clamp; we don't re-clamp here.
fn blend_by_cluster_score(
    pool: &[(usize, f32)],
    leaf_nodes: &[RaptorNode],
    query_embedding: &[f32],
    weight: f32,
) -> Vec<usize> {
    if pool.is_empty() {
        return Vec::new();
    }
    if leaf_nodes.is_empty() {
        // Atlas hasn't landed (PartiallyReady / MultiHopReady) — fall
        // back to cosine ordering. This is the graceful baseline the
        // spec calls out as a hard requirement.
        let mut by_cosine: Vec<(usize, f32)> = pool.to_vec();
        by_cosine.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        return by_cosine.into_iter().map(|(i, _)| i).collect();
    }

    // chunk_id (u32) → leaf node index. Built once per call; PPR
    // boosting + cosine top-K typically gives a pool of ~22-50,
    // each lookup is O(1).
    let mut chunk_to_cluster: std::collections::HashMap<u32, usize> =
        std::collections::HashMap::with_capacity(
            leaf_nodes.iter().map(|n| n.direct_member_chunk_ids.len()).sum(),
        );
    for (node_idx, node) in leaf_nodes.iter().enumerate() {
        for chunk_id in &node.direct_member_chunk_ids {
            chunk_to_cluster.insert(*chunk_id, node_idx);
        }
    }

    // Cache cluster→query cosine across candidates — multiple
    // chunks in the same cluster share a score, so cosine the
    // cluster summary embedding once per cluster, not per chunk.
    let mut cluster_score_cache: Vec<Option<f32>> = vec![None; leaf_nodes.len()];
    let mut cosines: Vec<f32> = Vec::with_capacity(pool.len());
    let mut clusters: Vec<f32> = Vec::with_capacity(pool.len());
    for (chunk_idx, cosine_score) in pool {
        cosines.push(*cosine_score);
        let cluster_score = match chunk_to_cluster.get(&(*chunk_idx as u32)) {
            Some(&node_idx) => match cluster_score_cache[node_idx] {
                Some(s) => s,
                None => {
                    let s = cosine_similarity(
                        query_embedding,
                        &leaf_nodes[node_idx].summary_embedding,
                    );
                    cluster_score_cache[node_idx] = Some(s);
                    s
                }
            },
            // Chunk has no leaf-cluster assignment (rare edge: a
            // chunk index outside any cluster's direct members).
            // Use the candidate-pool minimum so it neither helps
            // nor distinctively hurts — the cosine signal alone
            // decides this chunk's fate.
            None => 0.0,
        };
        clusters.push(cluster_score);
    }

    let cosine_norm = min_max_normalize(&cosines);
    let cluster_norm = min_max_normalize(&clusters);
    let mut scored: Vec<(usize, f32)> = pool
        .iter()
        .zip(cosine_norm.iter().zip(cluster_norm.iter()))
        .map(|((idx, _), (cn, kn))| (*idx, (1.0 - weight) * cn + weight * kn))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(idx, _)| idx).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn leaf_node(node_id: &str, chunks: Vec<u32>, summary_embedding: Vec<f32>) -> RaptorNode {
        RaptorNode {
            node_id: node_id.to_string(),
            level: 0,
            summary: format!("summary for {node_id}"),
            summary_embedding,
            centroid_embedding: vec![],
            children_node_ids: vec![],
            direct_member_chunk_ids: chunks.clone(),
            evidence_chunk_ids: chunks,
            quote_spans: vec![],
            primary_entities: vec![],
            cluster_coherence: 1.0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn min_max_normalize_handles_empty_slice() {
        assert!(min_max_normalize(&[]).is_empty());
    }

    #[test]
    fn min_max_normalize_handles_all_equal_returns_midpoint() {
        // All-equal cosines is a real pool shape — every chunk equally
        // close to the query (typical of broad thematic queries). The
        // normaliser must not divide by zero and must not collapse
        // every chunk to 0.0, which would let cluster_norm dictate
        // the ranking even when the user asked for `weight=0.25`.
        let out = min_max_normalize(&[0.7, 0.7, 0.7, 0.7]);
        assert_eq!(out, vec![0.5; 4]);
    }

    #[test]
    fn min_max_normalize_basic_scales_to_unit_interval() {
        let out = min_max_normalize(&[0.1, 0.5, 0.9]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 0.5).abs() < 1e-5);
        assert!((out[2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn blend_falls_through_to_cosine_when_no_leaf_nodes() {
        // PartiallyReady / MultiHopReady have no raptor_nodes yet.
        // The blend MUST return cosine ordering rather than panic
        // or collapse everything to zero.
        let pool = vec![(7, 0.2), (3, 0.9), (5, 0.5)];
        let ranked = blend_by_cluster_score(&pool, &[], &[1.0, 0.0], 0.5);
        // Expected order: highest cosine first.
        assert_eq!(ranked, vec![3, 5, 7]);
    }

    #[test]
    fn blend_handles_empty_pool() {
        let ranked = blend_by_cluster_score(&[], &[], &[0.0], 0.5);
        assert!(ranked.is_empty());
    }

    #[test]
    fn blend_at_zero_weight_yields_cosine_order() {
        // Spec invariant: when callers do disable the gate they
        // should NEVER call with weight=0.0 (we early-return at the
        // env gate). But the math should still be correct — proving
        // it here means a future refactor that removes the gate
        // doesn't silently change behaviour.
        let query = vec![1.0, 0.0];
        let leaves = vec![
            // Cluster 0 — orthogonal to query, low cluster_score.
            leaf_node("c0", vec![10, 11], vec![0.0, 1.0]),
            // Cluster 1 — perfectly aligned, high cluster_score.
            leaf_node("c1", vec![20, 21], vec![1.0, 0.0]),
        ];
        // Chunks in cluster 0 have higher cosine; chunks in cluster
        // 1 have lower cosine. At weight=0.0 the cosine winner (10)
        // must come out on top despite cluster 1 being a better
        // structural match.
        let pool = vec![(10, 0.9), (11, 0.7), (20, 0.2), (21, 0.1)];
        let ranked = blend_by_cluster_score(&pool, &leaves, &query, 0.0);
        assert_eq!(ranked, vec![10, 11, 20, 21]);
    }

    #[test]
    fn blend_at_full_weight_promotes_cluster_winner() {
        // Same fixture as above. At weight=1.0 the cosine signal is
        // out — cluster 1 (aligned) should dominate. Chunks 20 and
        // 21 share a cluster, so they tie on cluster_norm; the
        // sort is stable enough that BOTH must rank ahead of the
        // chunks in cluster 0. Exact order within a cluster falls
        // back to the original pool position; we only assert the
        // cluster-level promotion.
        let query = vec![1.0, 0.0];
        let leaves = vec![
            leaf_node("c0", vec![10, 11], vec![0.0, 1.0]),
            leaf_node("c1", vec![20, 21], vec![1.0, 0.0]),
        ];
        let pool = vec![(10, 0.9), (11, 0.7), (20, 0.2), (21, 0.1)];
        let ranked = blend_by_cluster_score(&pool, &leaves, &query, 1.0);
        let first_two: std::collections::HashSet<usize> =
            ranked.iter().take(2).copied().collect();
        assert_eq!(
            first_two,
            [20, 21].iter().copied().collect::<std::collections::HashSet<usize>>(),
            "weight=1.0 should rank both cluster-1 chunks ahead of cluster-0 chunks; got {ranked:?}"
        );
    }

    #[test]
    fn blend_at_intermediate_weight_lifts_cluster_winner_above_cosine() {
        // The realistic case: cosine ranks chunk 10 first, but
        // chunk 20 lives in the RIGHT neighbourhood (cluster 1).
        // The structurally on-target chunk should overtake the
        // cosine winner once the cluster signal has enough weight.
        // This is the failure mode the spec targets: T1 winnie_fate's
        // chunk 957 has middling cosine but lives in the load-bearing
        // ending cluster.
        //
        // Note on the symmetry trap: with a 2-chunk pool where the
        // cosine and cluster signals point in EXACTLY opposite
        // directions, both signals normalise to {0.0, 1.0} so any
        // weight ≠ 0.5 has a clear winner but weight = 0.5 ties.
        // We probe at 0.7 — past the symmetry inflection — to keep
        // the test resilient to floating-point sort tie-breaking.
        let query = vec![1.0, 0.0];
        let leaves = vec![
            // Cluster 0 — barely related to query.
            leaf_node("c0", vec![10], vec![0.1, 0.99]),
            // Cluster 1 — perfectly on-topic.
            leaf_node("c1", vec![20], vec![1.0, 0.05]),
        ];
        let pool = vec![(10, 0.6), (20, 0.4)];
        let ranked = blend_by_cluster_score(&pool, &leaves, &query, 0.7);
        assert_eq!(
            ranked[0], 20,
            "structurally on-target chunk should win at weight=0.7; got {ranked:?}"
        );
    }

    #[test]
    fn blend_uses_zero_cluster_score_for_unassigned_chunks() {
        // Defensive: a chunk in the pool that isn't a direct member
        // of any leaf cluster (edge case — partial atlas, off-by-one
        // chunk indexing) must not panic. It gets cluster_score=0.0,
        // so it gets penalised by the blend but doesn't poison the
        // rest of the pool.
        let query = vec![1.0, 0.0];
        let leaves = vec![leaf_node("c0", vec![10], vec![1.0, 0.0])];
        let pool = vec![(10, 0.6), (999, 0.5)]; // 999 is unassigned
        let ranked = blend_by_cluster_score(&pool, &leaves, &query, 0.9);
        // The assigned chunk's cluster_score is 1.0 → blend dominates;
        // the unassigned chunk's cluster_score is 0.0 → falls behind.
        assert_eq!(ranked[0], 10);
        assert_eq!(ranked[1], 999);
    }

    #[test]
    fn blend_does_not_divide_by_zero_on_all_equal_cosines() {
        // Failure mode #2 from the spec: a pool where every chunk
        // has identical cosine. min-max normalise on cosine alone
        // would divide by zero; here the cluster signal still has
        // variance, so the blend should produce a non-NaN ranking.
        let query = vec![1.0, 0.0];
        let leaves = vec![
            leaf_node("c0", vec![1], vec![0.0, 1.0]),
            leaf_node("c1", vec![2], vec![1.0, 0.0]),
        ];
        let pool = vec![(1, 0.5), (2, 0.5)];
        let ranked = blend_by_cluster_score(&pool, &leaves, &query, 0.5);
        // Cluster signal breaks the tie — chunk 2 (cluster 1, aligned
        // with query) wins.
        assert_eq!(ranked, vec![2, 1]);
    }
}
