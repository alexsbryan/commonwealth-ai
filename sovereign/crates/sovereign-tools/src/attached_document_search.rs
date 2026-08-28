// SPDX-License-Identifier: AGPL-3.0-or-later
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

use sovereign_core::error::{Error, Result};
use sovereign_core::tool_manifest::DeclaredTool;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::{AssetState, StepOutput, ToolContext};

pub struct AttachedDocumentSearchTool {
    store: Arc<dyn StateStore>,
    inference: Arc<dyn InferenceProvider>,
}

impl AttachedDocumentSearchTool {
    pub fn new(store: Arc<dyn StateStore>, inference: Arc<dyn InferenceProvider>) -> Self {
        Self { store, inference }
    }
}

impl AttachedDocumentSearchTool {
    /// Bind this tool's state to its `attached_doc_search` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        let run_state = Arc::clone(&state);
        sovereign_core::tool_manifest::declared("attached_doc_search", move |params, ctx| {
            let state = Arc::clone(&run_state);
            async move { state.run(&params, &ctx).await }
        })
        .with_validate({
            let state = Arc::clone(&state);
            Arc::new(move |p: &serde_json::Value| state.validate_extra(p))
        })
    }

    /// The executable half of `attached_doc_search`.
    async fn run(&self, params: &serde_json::Value, ctx: &ToolContext) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("missing `query`".to_string()))?;

        // ── Resolve the attached asset ───────────────────────────
        //
        // By conversation, deterministically: the turn's
        // `DocumentSession.source` carries the asset id (the same
        // contract `handle_attached_doc_turn` reads). This closes the
        // TODO on sovereign decision 7693f16b — the prior V1 stub
        // ("most-recently-ingested Ready asset") searched whichever
        // document was attached LAST anywhere in the store, so a
        // conversation pinned to asset A silently retrieved from
        // asset B once anything newer reached Ready (observed
        // 2026-07-23: every book-report question against the Conrad
        // asset retrieved the meridian-postmortem fixture).
        //
        // The stub survives only as a fallback for tool invocations
        // with no document session on the conversation.
        let session_asset = match self
            .store
            .get_document_session_by_conversation(&ctx.conversation_id)
            .await
        {
            Ok(Some(session)) => self.store.get_document_asset(&session.source).await?,
            _ => None,
        };
        let asset = match session_asset {
            Some(a) => a,
            None => {
                let assets = self.store.list_document_assets().await?;
                match assets
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
                }
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
        let ppr_enabled = std::env::var("SOVEREIGN_DOC_PPR")
            .map(|v| v != "off")
            .unwrap_or(true);
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
                        let cosine_top_indices: std::collections::HashSet<usize> =
                            scored.iter().take(cosine_top_k).map(|(_, i)| *i).collect();
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
                            b.1.partial_cmp(&a.1)
                                .unwrap_or(std::cmp::Ordering::Equal)
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
        // ── RAPTOR cluster-score blend — REJECTED 2026-07-31, removed 2026-08-01 ──
        //
        // `SOVEREIGN_DOC_CLUSTER_WEIGHT` / `SOVEREIGN_DOC_CLUSTER_POOL` are
        // gone. The knob shipped dark 2026-05-22 and the T1 P0.4 ablation
        // matrix settled it: Δ = 0.0000 on every sep bank, every rep
        // (`sovereign/bench/ablation/2026-07-31-sep-knob-matrix.json`), which
        // is the reject condition its own DEFAULTS_LEDGER row named. Scope
        // caveat kept honest: those banks do not exercise this attached-doc
        // path, so the evidence is "no bank can show this earns its keep",
        // not "it was measured here and lost". Re-open per the ledger row —
        // T2 P3.1 authoring a bank that drives attached-doc retrieval — and
        // recover the implementation from this commit's parent.
        //
        // The rationale is preserved in
        // `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`; deleting the code
        // while a Rejected verdict stood is the complexity ratchet
        // (`ENRICHMENT_ROADMAP.md:348`) actually moving: knobs 12 → 10.
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
            let chunk_content_opt = raw
                .iter()
                .find(|c| c.source == asset_source_key && c.chunk_index == *chunk_idx);
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

    fn validate_extra(&self, params: &serde_json::Value) -> Result<()> {
        match params.get("query").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => Ok(()),
            _ => Err(Error::InvalidInput(
                "attached_doc_search requires a non-empty `query` string".to_string(),
            )),
        }
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
