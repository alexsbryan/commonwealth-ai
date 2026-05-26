//! Retrieval pipeline — chunk-fetch, atlas grounding, source
//! expansion, conversation-tiered briefing, and the `prepare_knowledge_context`
//! orchestrator that drives Runtime's synthesis paths.
//!
//! Everything in this module is `impl Runtime` — the methods access
//! Runtime's engine handles (`corpus_engine`, `wikipedia_graph`,
//! `meta_atlas`, `atlas_context_provider`, `conv_tiered_reader`,
//! `landscape_digests`, etc.) — so the natural split is by concern
//! (retrieval vs. system message vs. handler) within the same struct,
//! not by struct boundary.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

use crate::error::Result;
use crate::traits::*;
use crate::types::*;

use super::*;

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
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        if std::env::var("SOVEREIGN_GRAPH_NEIGHBOR_EXPAND").ok().as_deref() != Some("1") {
            return None;
        }
        let graph = self.wikipedia_graph.as_ref()?;

        let already_present: std::collections::HashSet<String> = chunks
            .iter()
            .filter_map(|c| c.title.clone())
            .collect();

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
                if t.len() >= 4 && !["with", "their", "from", "into", "onto"].contains(&t.as_str()) {
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
    /// Snapshot the folder-metadata oracle. Returns an empty map
    /// when no oracle is wired (CLI fallback / tests), which makes
    /// every callee's `folder_metadata` lookup miss and so the
    /// pre-Phase-F label rendering applies. Folder-ingest v1 §6.3.
    pub(crate) async fn folder_metadata_snapshot(
        &self,
    ) -> std::collections::HashMap<String, crate::traits::FolderMetadata> {
        match &self.folder_metadata {
            Some(oracle) => oracle.folder_metadata().await,
            None => std::collections::HashMap::new(),
        }
    }
    /// Build the set of chunk titles whose Wikipedia source has at
    /// least one section flagged contested (`pov_count > 0` OR
    /// `section_type = "controversy"`). Used by
    /// `format_scored_chunks_with_kinds` to suffix `(contested)` on
    /// source labels. Returns an empty set when no graph is loaded —
    /// callers degrade gracefully.
    pub(crate) async fn contested_titles_for_chunks(
        &self,
        chunks: &[corpus_engine::ScoredChunk],
    ) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        let Some(graph) = self.wikipedia_graph.as_ref() else {
            return out;
        };
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for c in chunks {
            let Some(title) = c.title.clone() else {
                continue;
            };
            if !seen.insert(title.clone()) {
                continue;
            }
            if graph.has_contested_section(&title).await {
                out.insert(title);
            }
        }
        out
    }
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

        // Only fire on shapes where per-entity decomposition is
        // structurally meaningful. KnowledgeQuery is included
        // because the classifier sometimes routes "How do X and Y
        // differ on Z?" to KnowledgeQuery rather than
        // ComparisonQuery.
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
        eprintln!(
            "[query_decomp] entities={:?} axis={axis:?} queries={queries:?}",
            entities,
        );
        tracing::info!(
            entities = ?entities,
            axis = %axis,
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
    ) -> usize {
        let mut added = 0usize;
        for sq in sub_queries {
            let emb = self.inference.embed_query(sq).await.unwrap_or_default();
            let hits = self
                .search_corpus_indexes_with_overrides(
                    &emb,
                    sq,
                    DECOMP_QUERY_LIMIT,
                    label,
                    None,
                    enabled_corpora,
                )
                .await;
            added += hits.len();
            chunks.extend(hits);
        }
        added
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
            "Given the conversation and the user's current question, name 2-3 \
             specific Wikipedia article titles that would directly answer the \
             question. Use the exact title an English Wikipedia article would \
             have (e.g., \"Transistor\", \"Von Neumann architecture\", \
             \"History of computing hardware\"). If the question pivots to a \
             new topic, name the titles for the new topic — do not anchor on \
             the prior subject.\n\n\
             Recent conversation:\n{recent_summary}\n\n\
             Current question: {message}\n\n\
             Reply with JSON only:\n\
             {{\"titles\": [\"Title 1\", \"Title 2\"]}}"
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
    /// Re-rank conv-corpus chunks via Personalized PageRank over each
    /// conv's entity co-occurrence graph (built from RAPTOR
    /// `primary_entities` — zero new LLM cost). Spec
    /// `sovereign/docs/specs/CONV_TIERED_PORT.md` §"T2 via reused
    /// RAPTOR primary_entities".
    ///
    /// Mutates `chunks` in place:
    /// - For conv-category chunks whose conv has a graph + a non-empty
    ///   seed-entity match in the query, blends cosine and entity
    ///   mass: `score = (1-w)·cosine_norm + w·entity_norm`.
    /// - Re-sorts `chunks` by descending blended score so downstream
    ///   prompt assembly + briefing surface the entity-bridged hits
    ///   first.
    ///
    /// `SOVEREIGN_CONV_PPR_WEIGHT` env var overrides the default
    /// `0.25` weight (range `[0.0, 1.0]`). `0.0` short-circuits the
    /// entire path (no LLM, no graph builds) so production can be
    /// switched off without a recompile.
    pub(crate) async fn rerank_conv_chunks_via_ppr(
        &self,
        query: &str,
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        display_categories: &std::collections::HashMap<String, String>,
    ) {
        let weight = std::env::var("SOVEREIGN_CONV_PPR_WEIGHT")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .map(|w| w.clamp(0.0, 1.0))
            .unwrap_or(0.25_f32);
        if weight <= 0.0 {
            return;
        }
        let Some(reader) = self.conv_tiered_reader.as_ref() else {
            return;
        };

        // 1. Group tiered-corpus chunks by (corpus_id, conv_uuid).
        // Watched-folder corpora collapse all chunks under
        // `conv_uuid = corpus_id` (one graph per folder); conversation
        // corpora bucket by source_doc_id (one graph per conv). The
        // helper in `conv_briefing` centralises the dispatch so both
        // call sites stay in sync.
        let mut convs: std::collections::HashMap<(String, String), Vec<usize>> =
            std::collections::HashMap::new();
        for (idx, c) in chunks.iter().enumerate() {
            let Some(category) = display_categories.get(&c.corpus_id) else {
                continue;
            };
            if !crate::conv_briefing::is_tiered_category(category) {
                continue;
            }
            let Some(uuid) = crate::conv_briefing::tiered_group_key(
                category,
                &c.corpus_id,
                c.source_doc_id.as_deref(),
            ) else {
                continue;
            };
            convs
                .entry((c.corpus_id.clone(), uuid.to_string()))
                .or_default()
                .push(idx);
        }
        if convs.is_empty() {
            return;
        }

        // 2. Per-conv: build graph, run PPR, project to chunk mass.
        // `idx_to_mass` accumulates entity-mass score per chunk index
        // in `chunks`. Conv chunks not assigned mass (e.g. graph
        // didn't seed because no query entity matched) stay at 0 —
        // they fall to the bottom of the conv-chunks bucket after
        // blending without affecting non-conv chunks.
        let mut idx_to_mass: std::collections::HashMap<usize, f32> =
            std::collections::HashMap::new();
        let mut seeded_convs = 0usize;
        for ((corpus_id, conv_uuid), chunk_indices) in &convs {
            // Layered builder: combine BOTH RAPTOR primary_entities
            // (LLM-judged cluster-distinctiveness, ~5/leaf) AND
            // GliNER chunk_entities (raw NER recall, ~24/chunk).
            // Edge weights accumulate across layers; entities
            // present in both sources end up with the strongest
            // bonds. Empty collections collapse the unused layer
            // naturally — fully RAPTOR-only or fully GliNER-only
            // corpora both work without code branching here.
            let chunk_entity_rows = reader
                .list_chunk_entities_for_conv(corpus_id, conv_uuid)
                .await
                .unwrap_or_default();
            let raptor_nodes = reader
                .list_conv_raptor_nodes(corpus_id, conv_uuid)
                .await
                .unwrap_or_default();
            if chunk_entity_rows.is_empty() && raptor_nodes.is_empty() {
                continue;
            }
            tracing::debug!(
                corpus = corpus_id,
                conv = conv_uuid,
                chunk_entities = chunk_entity_rows.len(),
                raptor_nodes = raptor_nodes.len(),
                "conv_entity_graph: building layered (GliNER + RAPTOR)"
            );
            let graph = crate::conv_entity_graph::ConvEntityGraph::from_layered(
                corpus_id,
                conv_uuid,
                &raptor_nodes,
                &chunk_entity_rows,
            );
            if graph.is_empty() {
                continue;
            }
            let seeds = graph.seed_indices_from_query(query);
            if seeds.is_empty() {
                continue;
            }
            seeded_convs += 1;
            let mass = graph.personalized_pagerank(&seeds, 0.85, 20);
            let per_chunk_mass = graph.chunk_mass(&mass);
            // Top-contributing seed entity for provenance stamping
            // (A3-lite). The "credit" goes to whichever seed has
            // the highest PPR mass — that's the entity whose query-
            // match did the most diffusion work for this conv.
            let top_seed_name: Option<String> = seeds
                .iter()
                .max_by(|a, b| {
                    let ma = mass.get(**a).copied().unwrap_or(0.0);
                    let mb = mass.get(**b).copied().unwrap_or(0.0);
                    ma.partial_cmp(&mb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .and_then(|i| graph.entity_name(*i));
            for &idx in chunk_indices {
                if let Some(chunk_id) = chunks[idx].chunk_id {
                    if let Some(&m) = per_chunk_mass.get(&chunk_id) {
                        idx_to_mass.insert(idx, m);
                        if let Some(seed) = top_seed_name.as_ref() {
                            chunks[idx]
                                .metadata
                                .insert("ppr_seed".to_string(), seed.clone());
                        }
                    }
                }
            }
        }
        if idx_to_mass.is_empty() {
            return;
        }

        // 3. Min-max normalise cosine + entity mass across the
        // conv-chunk pool. We touch ONLY conv chunks — non-conv
        // chunks keep their original score so cross-corpus ranking
        // stays stable.
        let conv_indices: Vec<usize> = convs.values().flatten().copied().collect();
        let (cosine_min, cosine_max) =
            conv_indices.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &i| {
                let s = chunks[i].score;
                (lo.min(s), hi.max(s))
            });
        let cosine_range = (cosine_max - cosine_min).max(1e-6);

        let (mass_min, mass_max) = idx_to_mass.values().copied().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(lo, hi), v| (lo.min(v), hi.max(v)),
        );
        let mass_range = (mass_max - mass_min).max(1e-6);

        for &i in &conv_indices {
            let cos_norm = if cosine_max > cosine_min {
                (chunks[i].score - cosine_min) / cosine_range
            } else {
                0.5
            };
            let raw_mass = idx_to_mass.get(&i).copied().unwrap_or(0.0);
            let entity_norm = if mass_max > 0.0 {
                (raw_mass - mass_min) / mass_range
            } else {
                0.0
            };
            chunks[i].score = (1.0 - weight) * cos_norm + weight * entity_norm;
            // Provenance stamp (A3-lite). The frontend uses
            // `ppr_mass_norm` to gate the "↗ surfaced via entity
            // bridge" subtitle; only chunks with `ppr_seed` AND
            // `ppr_mass_norm > 0.5` render the badge. Other conv
            // chunks (cosine-only ranked, mass=0) stay unbadged.
            if idx_to_mass.contains_key(&i) {
                chunks[i].metadata.insert(
                    "ppr_mass_norm".to_string(),
                    format!("{:.3}", entity_norm),
                );
            }
        }

        // 4. Re-sort descending. Non-conv chunks were left untouched
        // (their original `score` semantics survive), but the global
        // sort still orders the conv chunks correctly amongst
        // themselves and lets entity-boosted conv hits float above
        // unrelated chunks where appropriate.
        chunks.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        tracing::debug!(
            weight,
            seeded_convs,
            blended_chunks = idx_to_mass.len(),
            "conv_entity_graph: PPR rerank applied"
        );
    }
    /// Render the conversation-tiered briefing block for the retrieval
    /// hit set. Returns an empty string when no reader is wired or no
    /// conv-category chunks made the cut — preserves the pre-tiered
    /// prompt layout exactly in those cases.
    ///
    /// Two-part output:
    ///   1. Per-source briefing (conv / vault / watched_folder) — the
    ///      pre-existing tiered surface.
    ///   2. Vault-wide synthesis briefing — cross-note themes from
    ///      `vault_themes`, present only when vault-category chunks are
    ///      in the hit set AND themes intersect the hit notes.
    ///
    /// Stitched in the canonical order: per-source first (more
    /// concrete, more retrievable-back) then synthesis (broader
    /// pattern context for the synth model to ground generalisations).
    pub(crate) async fn build_conv_briefing_block(
        &self,
        chunks: &[corpus_engine::ScoredChunk],
        display_categories: &std::collections::HashMap<String, String>,
    ) -> String {
        let Some(reader) = self.conv_tiered_reader.as_ref() else {
            return String::new();
        };
        let cats_opt = if display_categories.is_empty() {
            None
        } else {
            Some(display_categories)
        };
        let payload = crate::conv_briefing::build_conv_tiered_briefings(
            reader, chunks, cats_opt,
        )
        .await;
        if !payload.rendered.is_empty() {
            tracing::debug!(
                mode = payload.mode.label(),
                convs = payload.conv_count,
                "conv_briefing: surfaced tiered context"
            );
        }
        let vault_block = crate::conv_briefing::build_vault_synthesis_briefings(
            reader, chunks, cats_opt,
        )
        .await;
        if !vault_block.is_empty() {
            tracing::debug!(
                bytes = vault_block.len(),
                "conv_briefing: surfaced vault synthesis themes"
            );
        }
        if payload.rendered.is_empty() {
            vault_block
        } else if vault_block.is_empty() {
            payload.rendered
        } else {
            format!("{}\n{}", payload.rendered, vault_block)
        }
    }
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

        let mut corpus_ids = provider.loaded_corpus_ids();
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
            corpus_ids.retain(|id| {
                PERSONAL_CORPUS_PREFIXES.iter().any(|p| id.starts_with(p))
            });
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
        let ctxs: Vec<Arc<crate::atlas_context::AtlasContext>> =
            corpus_ids.iter().filter_map(|id| provider.get(id)).collect();
        let graphs: Vec<Arc<crate::atlas_context::AtlasGraph>> =
            corpus_ids.iter().filter_map(|id| provider.graph(id)).collect();

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
            let requests = crate::atlas_context::atlas_navigate(
                query_text,
                embedding,
                &ctx_refs,
                &graph_refs,
                max_seeds,
                /*max_hops=*/ 2,
            );
            // Production budget mirrors the eval-CLI's calibrated
            // value (limit * 0.6, where limit is `KQ_PER_CORPUS_LIMIT
            // = 20`). Calibrated against the SEP bank: budget=6 gave
            // +22 sources / +6 essay / +6 dialectical_breadth vs
            // baseline; budget=4 left ~10 bank-required articles
            // unfetched even when their atlas was loaded.
            let fetch_budget = ((KQ_PER_CORPUS_LIMIT as f32) * 0.6).ceil() as usize;
            let mut graph_added = 0usize;
            let mut seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for req in requests.iter().take(fetch_budget * 2) {
                if graph_added >= fetch_budget {
                    break;
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
                    if let Some(mut boosted) =
                        self.fetch_chunk_by_id(&req.article_slug, chunk_id_num).await
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
                            boosted.vector_distance = Some(
                                (1.0_f32 - (req.score / 2.0).min(1.0)).max(0.0),
                            );
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

                // SEP/Wikipedia article-slug path.
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
                        enabled_corpora,
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
                    let virt =
                        crate::atlas_context::atlas_top_k_as_chunks(embedding, &ctx);
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
    /// Search every installed knowledge/catalog corpus with optional
    /// per-corpus K overrides (hot-corpora affinity pre-merge bias).
    /// When the conversation has already drawn many chunks from a
    /// corpus, we increase its candidate pool so the merge layer
    /// sees more of its top results. Per-corpus K defaults to
    /// `limit` for any corpus not in the override map.
    ///
    /// `enabled_corpora` is the user-controlled per-conversation
    /// allow-list (`Conversation::enabled_corpora`). `None` means
    /// "no filter — search every installed corpus" (the default
    /// behavior). `Some(allow)` drops every index whose `corpus_id`
    /// is absent from the allow-list, with one twist: an index whose
    /// `parent_corpus_id` is in the list is kept (layer/satellite
    /// corpora follow their parent). The filter applies AFTER the
    /// existing kind/dim/sensitivity filters so they can short-circuit
    /// without inspecting the allow-list.
    pub(crate) async fn search_corpus_indexes_with_overrides(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        label: &str,
        per_corpus_limits: Option<&HashMap<String, usize>>,
        enabled_corpora: Option<&[String]>,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None — no corpus search possible");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };
        if indexes.is_empty() {
            tracing::warn!("{label}: installed_indexes() returned 0 indexes — nothing to search");
        } else {
            tracing::info!(count = indexes.len(), "{label}: found corpus indexes");
        }

        // Filter 1 — drop Code corpora; keep Knowledge + Catalog.
        //
        // Code indexes (produced by `sovereign code index`) are served
        // by the dedicated symbol_lookup / code_search MCP tools;
        // pulling them into chat retrieval lets BM25 keyword overlap
        // on tokens like `main`, `argument`, or `democracy` drown out
        // the actual knowledge corpus for the turn.
        //
        // Catalog corpora are kept — they're the primary signal for
        // "system knows of this work but hasn't read it yet." The
        // synthesis prompt has a CATALOG-AWARE section that tells
        // the model how to handle them (no confabulation, end with
        // an ingest offer). `format_scored_chunks` buckets them
        // into a separate evidence tier downstream.
        let total_indexes = indexes.len();
        let indexes: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                if matches!(
                    info.kind,
                    corpus_engine::CorpusKind::Knowledge
                        | corpus_engine::CorpusKind::Catalog
                ) {
                    true
                } else {
                    tracing::debug!(
                        corpus = %info.corpus_id,
                        kind = ?info.kind,
                        "{label}: skipping code corpus for chat retrieval"
                    );
                    false
                }
            })
            .collect();
        if indexes.len() < total_indexes {
            tracing::info!(
                knowledge = indexes.len(),
                code_skipped = total_indexes - indexes.len(),
                "{label}: filtered code corpora"
            );
        }

        // Filter 2 — drop dimension mismatches. A corpus built with
        // a different embedding model can't serve hybrid search for
        // the current query. When the query embedding is empty
        // (FTS-only path), skip this filter so every remaining
        // (knowledge) index serves its BM25 results.
        let query_dims = embedding.len();
        let total_indexes = indexes.len();
        let eligible: Vec<_> = if query_dims == 0 {
            indexes
        } else {
            indexes
                .into_iter()
                .filter(|info| {
                    if info.embedding_dimensions == query_dims {
                        true
                    } else {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            stored_dims = info.embedding_dimensions,
                            query_dims,
                            embedding_model = %info.embedding_model,
                            "{label}: skipping corpus — embedding-dimension mismatch"
                        );
                        false
                    }
                })
                .collect()
        };
        if eligible.len() < total_indexes {
            tracing::info!(
                eligible = eligible.len(),
                skipped = total_indexes - eligible.len(),
                query_dims,
                "{label}: dim-filtered index set"
            );
        }

        // Filter 3 — drop sensitive corpora from ambient retrieval.
        //
        // Folder-ingest v1 §3.4: a watched-folder corpus marked
        // sensitive is structurally absent from the agent's pre-turn
        // ambient context. This is the runtime-side enforcement
        // layer; sovereign-tools' `WatchedFolderConfig.sensitive`
        // flag and its on-disk state-file mirror are the other
        // layers (ARCH §7.4 defence in depth). When no oracle is
        // wired (tests, pre-v1 builds), this filter is a no-op and
        // every corpus passes through.
        //
        // Sensitivity composes with skill-level local_only
        // suppression, but they're orthogonal: local_only is a
        // categorical skill gate; sensitivity is per-corpus and
        // applies in every register that does ambient retrieval.
        let eligible_pre_sensitivity = eligible.len();
        let eligible: Vec<_> = if let Some(oracle) = &self.sensitive_corpora {
            let sensitive_ids = oracle.sensitive_corpus_ids().await;
            if sensitive_ids.is_empty() {
                eligible
            } else {
                eligible
                    .into_iter()
                    .filter(|info| {
                        if sensitive_ids.contains(&info.corpus_id) {
                            tracing::debug!(
                                corpus = %info.corpus_id,
                                "{label}: skipping sensitive corpus — excluded from ambient retrieval"
                            );
                            false
                        } else {
                            true
                        }
                    })
                    .collect()
            }
        } else {
            eligible
        };
        if eligible.len() < eligible_pre_sensitivity {
            tracing::info!(
                eligible = eligible.len(),
                sensitive_skipped = eligible_pre_sensitivity - eligible.len(),
                "{label}: sensitivity-filtered index set"
            );
        }

        // Filter 4 — user-controlled per-conversation allow-list. Layer
        // corpora (info.parent_corpus_id matches an allowed parent) are
        // retained automatically so toggling Wikipedia ON enables
        // Wikipedia + its newsworthy/recent-events layers in one click.
        let eligible_pre_allow = eligible.len();
        let eligible = apply_corpus_allow_list(eligible, enabled_corpora);
        if eligible.len() < eligible_pre_allow {
            tracing::info!(
                eligible = eligible.len(),
                allow_skipped = eligible_pre_allow - eligible.len(),
                "{label}: corpus allow-list filtered index set"
            );
        }

        for info in &eligible {
            tracing::info!(
                corpus = %info.corpus_id,
                path = %info.path.display(),
                chunks = info.chunk_count,
                dims = info.embedding_dimensions,
                embedding_model = %info.embedding_model,
                "{label}: opening index"
            );
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: open_index failed");
                    continue;
                }
            };
            let effective_limit = per_corpus_limits
                .and_then(|m| m.get(&info.corpus_id).copied())
                .unwrap_or(limit);
            if effective_limit != limit {
                tracing::info!(
                    corpus = %info.corpus_id,
                    base_limit = limit,
                    effective_limit,
                    "{label}: per-corpus K override applied"
                );
            }
            match idx
                .search_with_rerank(
                    embedding,
                    query_text,
                    effective_limit,
                    self.rerank_fn.as_ref(),
                    &self.rerank_config,
                    None,
                )
                .await
            {
                Ok(scored) => {
                    tracing::info!(
                        corpus = %info.corpus_id,
                        results = scored.len(),
                        rerank_enabled = self.rerank_config.enabled
                            && self.rerank_fn.is_some(),
                        "{label}: search complete"
                    );
                    // Naturalistic audit: top-3 per corpus so post-mortem
                    // can answer "did the right article even reach the
                    // merge pool from this corpus?" before any cap or
                    // expansion. Keeps the existing info!() line above
                    // unchanged; this is a sibling event on its own target.
                    let top3: Vec<(String, f32)> = scored
                        .iter()
                        .take(3)
                        .map(|c| {
                            (
                                c.title.clone().unwrap_or_default(),
                                c.score,
                            )
                        })
                        .collect();
                    tracing::info!(
                        target: "retrieval_audit",
                        event = "corpus_results",
                        label = label,
                        corpus = %info.corpus_id,
                        count = scored.len(),
                        effective_limit,
                        top3 = ?top3,
                        "retrieval_audit: corpus_results"
                    );
                    chunks.extend(scored);
                }
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: search failed");
                }
            }
        }

        // Merged-pool diversity (glassbox, OPT-IN). After fan-out + merge
        // this is the candidate set synthesis will see. The regressed
        // bench title-coverage metric scores against the DISTINCT source
        // titles present here, so logging the merged distinct-title count
        // + the titles makes "did the expected articles survive the
        // merge?" answerable from logs alone. Pairs with the per-corpus
        // `rerank_diversity` event emitted inside search_with_rerank.
        //
        // Gated on the `retrieval_audit` target: when off (production
        // default) we pay one atomic level-check and skip the dedup pass
        // + title clones entirely. The work only runs under
        // `retrieval_audit=info`.
        if tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO) {
            use std::collections::{HashMap, HashSet};
            let mut seen = HashSet::new();
            let mut distinct_titles: Vec<String> = Vec::new();
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            for c in &chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                let t = c.title.clone().unwrap_or_default();
                if seen.insert(t.clone()) {
                    distinct_titles.push(t);
                }
            }
            // Chunk counts per corpus, busiest first — the at-a-glance
            // cross-corpus-contamination signal (e.g. a wikipedia-target
            // turn whose pool is mostly `sep` chunks). This fan-out is
            // the shared retrieval entry point, so this single event
            // covers every handler — KnowledgeQuery, ComparisonQuery,
            // AND the DeepQuery/Simple path that has no turn_summary.
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            // Truncated query so events correlate to the bench question
            // without threading an id through every call site.
            let query_preview: String = query_text.chars().take(80).collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "merged_pool",
                label = label,
                query = %query_preview,
                merged_chunks = chunks.len(),
                distinct_titles = distinct_titles.len(),
                corpora_searched = eligible.len(),
                by_corpus = ?corpus_pairs,
                titles = ?distinct_titles,
                "retrieval_audit: merged_pool"
            );
        }
        chunks
    }
    /// Search a *specific subset* of installed corpora — the
    /// metalingual companion to [`search_corpus_indexes`].
    ///
    /// Two filter axes:
    /// - `kind_filter`: if `Some`, restrict to that `CorpusKind`
    ///   (e.g. `Code` for SystemCode locators). If `None`, allow all
    ///   kinds (Knowledge + Code + Catalog).
    /// - `name_match`: if `Some`, restrict to corpora whose
    ///   `corpus_id` or `corpus_name` *contains* the substring (case-
    ///   insensitive). Used to resolve NamedSource locators like
    ///   "according to SEP" → only the `sep` corpus.
    ///
    /// Empty result is meaningful — caller treats it as "no source
    /// for this locator is indexed" and surfaces that to the user.
    pub(crate) async fn search_corpora_filtered(
        &self,
        embedding: &[f32],
        query_text: &str,
        limit: usize,
        kind_filter: Option<corpus_engine::CorpusKind>,
        name_match: Option<&str>,
        label: &str,
        enabled_corpora: Option<&[String]>,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let mut chunks = Vec::new();
        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => {
                tracing::warn!("{label}: corpus_engine is None");
                return chunks;
            }
        };
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(error = %e, "{label}: installed_indexes() failed");
                return chunks;
            }
        };

        let name_lower = name_match.map(str::to_lowercase);
        let eligible: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                let kind_ok = match kind_filter {
                    Some(k) => info.kind == k,
                    None => true,
                };
                let name_ok = match &name_lower {
                    Some(needle) => {
                        info.corpus_id.to_lowercase().contains(needle)
                            || info.corpus_name.to_lowercase().contains(needle)
                    }
                    None => true,
                };
                kind_ok && name_ok
            })
            .filter(|info| {
                // Dim filter — skip embedding-mismatched corpora when
                // we have an embedding to compare against. Mirrors
                // search_corpus_indexes's filter 2.
                embedding.is_empty() || info.embedding_dimensions == embedding.len()
            })
            .collect();
        // Per-conversation allow-list — drop indexes the user has
        // toggled off. Layer corpora follow their parent's state.
        // See `apply_corpus_allow_list` for the parent-aware filter
        // contract.
        let eligible = apply_corpus_allow_list(eligible, enabled_corpora);

        if eligible.is_empty() {
            tracing::info!(
                kind_filter = ?kind_filter,
                name_match = ?name_match,
                "{label}: no eligible corpora after filter"
            );
            return chunks;
        }

        for info in &eligible {
            tracing::info!(
                corpus = %info.corpus_id,
                kind = ?info.kind,
                "{label}: opening filtered index"
            );
            let idx = match engine.open_index(&info.path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: open_index failed");
                    continue;
                }
            };
            match idx
                .search_with_rerank(
                    embedding,
                    query_text,
                    limit,
                    self.rerank_fn.as_ref(),
                    &self.rerank_config,
                    None,
                )
                .await
            {
                Ok(scored) => {
                    chunks.extend(scored);
                }
                Err(e) => {
                    tracing::warn!(corpus = %info.corpus_id, error = %e, "{label}: search failed");
                }
            }
        }
        chunks
    }
    /// Canonical-entity boost (Move 4). For every question entity that
    /// resolves through the cross-corpus
    /// [`corpus_engine::meta_atlas::MetaAtlasIndex`], pick the top
    /// anchor per articulation axis (max 3 — one per
    /// `Inventory|Argument|Trace`), run a focused per-corpus search
    /// against that anchor's corpus, inject the returned chunks into
    /// `chunks` with a small score lift that survives
    /// `KQ_MERGED_LIMIT` truncation, and tag each injected chunk's
    /// metadata with `articulation` + `stability`. Returns one
    /// [`MetaAtlasHitRecord`] per anchor.
    ///
    /// Why one anchor per axis rather than "primary + alts": the
    /// per-atom articulation classifier (Move 5 Stage 1) tags each
    /// anchor with what kind of epistemic content it holds. The
    /// chat-path goal is the synthesis model seeing structural map +
    /// articulated claim + lived practice as distinct prompt
    /// sections. Picking by axis preserves that legibility.
    ///
    /// `min_axis_weight` is the threshold the dominant axis must
    /// clear for an anchor to claim a slot. Anchors with weak
    /// dominance (ambiguous) are suppressed — better to inject
    /// nothing than to inject a chunk the classifier wasn't sure
    /// about.
    pub(crate) async fn meta_atlas_boost(
        &self,
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        entities: &[String],
        enabled_corpora: Option<&[String]>,
    ) -> Vec<MetaAtlasHitRecord> {
        let Some(index) = self.meta_atlas.as_ref() else {
            return Vec::new();
        };
        if index.is_empty() || entities.is_empty() {
            return Vec::new();
        }

        let matches = index.lookup_any(entities);
        if matches.is_empty() {
            return Vec::new();
        }

        // Reference score above which boosted chunks should sort.
        let top_score = chunks
            .iter()
            .map(|c| c.score)
            .fold(f32::MIN, f32::max)
            .max(1.0);

        let mut applied: Vec<MetaAtlasHitRecord> = Vec::new();
        let mut rank: usize = 0;
        const MIN_AXIS_WEIGHT: f32 = 0.40;

        for atom in matches {
            let entity_emb = self
                .inference
                .embed_query(&atom.display)
                .await
                .unwrap_or_default();
            if entity_emb.is_empty() {
                tracing::warn!(
                    entity = %atom.display,
                    "meta_atlas_boost: empty embedding for entity; skipping"
                );
                continue;
            }

            for axis in
                corpus_engine::stream_axes::Articulation::ALL.iter()
            {
                let anchor = match corpus_engine::meta_atlas::MetaAtlasIndex
                    ::top_anchor_for_axis(&atom, *axis, MIN_AXIS_WEIGHT)
                {
                    Some(a) => a,
                    None => continue,
                };
                let hits = self
                    .search_corpora_filtered(
                        &entity_emb,
                        &atom.display,
                        CANONICAL_PRIMARY_LIMIT,
                        None,
                        Some(&anchor.corpus_id),
                        "MetaAtlasBoost",
                        enabled_corpora,
                    )
                    .await;
                let stability_tag = anchor
                    .stability
                    .map(|s| s.as_str().to_string());
                let added = inject_meta_atlas_hits(
                    chunks,
                    hits,
                    &anchor.corpus_id,
                    axis.as_str(),
                    stability_tag.as_deref(),
                    top_score,
                    &mut rank,
                );
                applied.push(MetaAtlasHitRecord {
                    entity: atom.display.clone(),
                    corpus_id: anchor.corpus_id.clone(),
                    articulation: axis.as_str().to_string(),
                    stability: stability_tag,
                    chunks_added: added,
                });
            }
        }

        applied
    }
    /// Source-cohesion expansion.
    ///
    /// When the initial retrieval has clearly landed on a single
    /// dominant document, the best next move is to read THAT DOCUMENT,
    /// not to scatter across marginal matches from other corpora. This
    /// fetches up to `EXPANSION_MAX_FROM_TOP_SOURCE` chunks from the
    /// dominant source by exact title, merges them with the initial
    /// retrieval, dedupes by content, and keeps
    /// `EXPANSION_GROUNDING_CHUNKS` top-scoring non-dominant chunks
    /// for breadth.
    ///
    /// Returns the expanded chunk set (ready to feed to synthesis) and
    /// a structured event-shape tuple `(from_source, grounding,
    /// dropped_noise)` for glass-box logging.
    ///
    /// Preconditions: caller has computed an `EvidenceShape` and
    /// decided this case warrants expansion (FastFocused route +
    /// `top_source_repeat_count >= 2`). This function does not re-check
    /// those conditions — it just expands.
    pub(crate) async fn expand_from_dominant_source(
        &self,
        initial: Vec<corpus_engine::ScoredChunk>,
        shape: &EvidenceShape,
    ) -> (Vec<corpus_engine::ScoredChunk>, usize, usize, usize) {
        use std::collections::HashSet;

        let (top_corpus_id, top_title) = &shape.top_source_key;
        if top_corpus_id.is_empty() || top_title.is_empty() {
            // Nothing to expand — return initial unchanged.
            return (initial, 0, 0, 0);
        }

        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => return (initial, 0, 0, 0),
        };

        // Find the corpus's index path.
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — installed_indexes() failed"
                );
                return (initial, 0, 0, 0);
            }
        };
        let info = match indexes.iter().find(|i| &i.corpus_id == top_corpus_id) {
            Some(i) => i.clone(),
            None => {
                tracing::warn!(
                    top_corpus_id,
                    "KnowledgeQuery: source expansion skipped — corpus not found"
                );
                return (initial, 0, 0, 0);
            }
        };
        let idx = match engine.open_index(&info.path).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(
                    top_corpus_id,
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — open_index failed"
                );
                return (initial, 0, 0, 0);
            }
        };

        // Fetch by title. The score on returned chunks is uniform 1.0
        // (cohesion pull, not query-similarity) — don't confuse these
        // with RRF-scored search results.
        let t_fetch = std::time::Instant::now();
        let fetched = match idx
            .fetch_chunks_by_title(top_title, EXPANSION_MAX_FROM_TOP_SOURCE)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    top_corpus_id,
                    top_title,
                    error = %e,
                    "KnowledgeQuery: source expansion skipped — fetch_chunks_by_title failed"
                );
                return (initial, 0, 0, 0);
            }
        };
        let fetch_ms = t_fetch.elapsed().as_millis() as u64;

        // Dedupe: track contents we've already seen. The initial
        // retrieval's dominant-source chunks will collide with some of
        // the fetched ones — keep the fetched copy (which is in natural
        // document order) and drop the duplicates.
        let mut seen_contents: HashSet<String> = HashSet::new();
        let mut expanded_dominant: Vec<corpus_engine::ScoredChunk> = Vec::new();
        for c in fetched {
            if seen_contents.insert(c.content.clone()) {
                expanded_dominant.push(c);
            }
        }

        // From the initial retrieval, keep up to
        // EXPANSION_GROUNDING_CHUNKS chunks that are NOT from the
        // dominant source, in descending score order. These are the
        // "grounding" signals — "other sources discuss this too."
        //
        // Two classes of non-dominant chunks are skipped even when
        // they'd fit the budget:
        //
        // 1. `conversation-history` corpus chunks. These are previous
        //    user/assistant turns — a user message that happens to
        //    vector-match "Can you tell me about X" phrasing is not a
        //    corroborating source for a knowledge query, it's phrase
        //    noise. Including it invites the model to acknowledge it
        //    as a topical source and waste output tokens (observed on
        //    the Joan Robinson turn: a Schrödinger-PDF user message
        //    made the model append "Note: The question about
        //    summarizing Erwin Schrödinger's *..." and truncate
        //    against the 600-token cap).
        //
        // 2. Untitled chunks (empty `title`). Real knowledge sources
        //    have titles. Untitled rows are almost always raw
        //    messages, system fragments, or extraction artifacts —
        //    not sources worth citing.
        let dominant_key = shape.top_source_key.clone();
        let mut grounding: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut dropped_noise = 0usize;
        let mut dropped_conversation_history = 0usize;
        let mut dropped_untitled = 0usize;
        for c in &initial {
            let key = (
                c.corpus_id.clone(),
                c.title.clone().unwrap_or_default(),
            );
            if key == dominant_key {
                continue; // already expanded
            }
            // Source-quality filter. See `is_grounding_candidate`.
            if c.corpus_id == "conversation-history" {
                dropped_conversation_history += 1;
                continue;
            }
            if !is_grounding_candidate(c) {
                dropped_untitled += 1;
                continue;
            }
            if grounding.len() < EXPANSION_GROUNDING_CHUNKS
                && seen_contents.insert(c.content.clone())
            {
                grounding.push(c.clone());
            } else {
                dropped_noise += 1;
            }
        }

        // Final ordering: dominant source FIRST (natural document
        // order, which maximises narrative coherence), grounding
        // second. The synthesis prompt template doesn't care about
        // ordering semantically but putting the dominant content up
        // top keeps it inside the truncate budget on small context
        // windows.
        let from_source = expanded_dominant.len();
        let grounding_kept = grounding.len();
        let mut merged = expanded_dominant;
        merged.extend(grounding);

        tracing::info!(
            top_source = %shape.top_source_label,
            initial_from_source = shape.top_source_repeat_count,
            additional_fetched = from_source.saturating_sub(shape.top_source_repeat_count),
            total_from_source = from_source,
            grounding_kept,
            dropped_noise,
            dropped_conversation_history,
            dropped_untitled,
            fetch_ms,
            "KnowledgeQuery: source expansion"
        );

        (merged, from_source, grounding_kept, dropped_noise)
    }
    /// Multi-source cohesion expansion — the synthesis-class sibling of
    /// [`expand_from_dominant_source`].
    ///
    /// **Additive, not replacive.** Earlier iteration of this expander
    /// replaced the initial top-K with title-fetched chunks from the
    /// top N source groups, on the theory that depth-from-canonical
    /// articles beat width-from-mixed-articles. Empirically that lost
    /// expected-source coverage on bank rows where the canonical
    /// articles ranked 5th-7th in the merged set: those articles got
    /// squeezed out of the top-N selection and disappeared from the
    /// prompt entirely. The bank measures sources-matched against the
    /// chunk titles in the prompt, so any breadth loss reads as a
    /// regression.
    ///
    /// The additive form: keep every chunk in `initial`, then *top up*
    /// each of the top `EXPANSION_MULTI_SOURCE_GROUPS` source groups
    /// to `EXPANSION_MULTI_PER_SOURCE` chunks by fetching the missing
    /// ones via title. Sources already at-or-above quota stay as-is;
    /// sources below quota gain depth without anyone losing breadth.
    /// Total chunk count grows from the initial set; the formatter
    /// downstream truncates at `EXPANDED_KNOWLEDGE_CHARS`, so
    /// over-generous fetches don't blow the prompt — they just give
    /// the formatter more material to choose from.
    ///
    /// Returns `(expanded_chunks, sources_expanded, chunks_added)`
    /// where `sources_expanded` is the number of groups that received
    /// at least one fetched chunk, and `chunks_added` is the gross
    /// number of new chunks added (after dedupe).
    pub(crate) async fn expand_from_top_sources(
        &self,
        initial: Vec<corpus_engine::ScoredChunk>,
    ) -> (Vec<corpus_engine::ScoredChunk>, usize, usize) {
        use std::collections::{HashMap, HashSet};

        let engine = match &self.corpus_engine {
            Some(e) => e,
            None => return (initial, 0, 0),
        };

        // Tally each (corpus_id, title) group's existing chunk count
        // and best score within the initial set. The best-score is
        // what ranks groups for top-N selection; the count is what
        // determines how many more we still need to fetch to reach
        // EXPANSION_MULTI_PER_SOURCE.
        let mut group_score: HashMap<(String, String), f32> = HashMap::new();
        let mut group_count: HashMap<(String, String), usize> = HashMap::new();
        let mut existing_contents: HashSet<(String, String)> = HashSet::new();
        for c in &initial {
            existing_contents.insert((c.corpus_id.clone(), c.content.clone()));
            if c.corpus_id == "conversation-history" {
                continue;
            }
            let title = c.title.as_deref().unwrap_or("").trim();
            if title.is_empty() {
                continue;
            }
            let key = (c.corpus_id.clone(), title.to_string());
            *group_count.entry(key.clone()).or_insert(0) += 1;
            let entry = group_score.entry(key).or_insert(c.score);
            if c.score > *entry {
                *entry = c.score;
            }
        }
        if group_score.len() < 2 {
            // Single-source-or-empty — single-source expander handles
            // the dominant case and we have nothing to multi-fetch.
            return (initial, 0, 0);
        }

        // Pick top N groups by best score.
        let mut groups: Vec<((String, String), f32)> = group_score.into_iter().collect();
        groups.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        groups.truncate(EXPANSION_MULTI_SOURCE_GROUPS);

        // Resolve corpus paths once.
        let indexes = match engine.installed_indexes().await {
            Ok(ix) => ix,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "multi-source expansion skipped — installed_indexes() failed"
                );
                return (initial, 0, 0);
            }
        };
        let path_for: HashMap<String, std::path::PathBuf> = indexes
            .iter()
            .map(|i| (i.corpus_id.clone(), i.path.clone()))
            .collect();

        // For each top group, top up to EXPANSION_MULTI_PER_SOURCE.
        // `fetch_chunks_by_title` returns chunks in natural document
        // order; we discard ones already present (by content equality
        // within the same corpus) and append the rest to the merged
        // result. Errors on a single group skip that group.
        let t_fetch = std::time::Instant::now();
        let mut merged = initial; // start from initial — additive!
        let mut sources_expanded = 0usize;
        let mut chunks_added = 0usize;
        for (key, _) in &groups {
            let already = group_count.get(key).copied().unwrap_or(0);
            if already >= EXPANSION_MULTI_PER_SOURCE {
                continue; // group already at quota; don't waste fetch
            }
            let need = EXPANSION_MULTI_PER_SOURCE - already;
            let Some(path) = path_for.get(&key.0) else {
                tracing::warn!(corpus = %key.0, "multi-source expansion: corpus path not found");
                continue;
            };
            let idx = match engine.open_index(path).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(corpus = %key.0, error = %e, "multi-source expansion: open_index failed");
                    continue;
                }
            };
            // Fetch the full quota — the dedupe loop below drops the
            // ones already present, leaving us with up to `need` net
            // additions per group.
            match idx
                .fetch_chunks_by_title(&key.1, EXPANSION_MULTI_PER_SOURCE)
                .await
            {
                Ok(group_chunks) => {
                    let mut added_this_group = 0usize;
                    for c in group_chunks {
                        if added_this_group >= need {
                            break;
                        }
                        let id = (c.corpus_id.clone(), c.content.clone());
                        if existing_contents.insert(id) {
                            merged.push(c);
                            chunks_added += 1;
                            added_this_group += 1;
                        }
                    }
                    if added_this_group > 0 {
                        sources_expanded += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        corpus = %key.0,
                        title = %key.1,
                        error = %e,
                        "multi-source expansion: fetch_chunks_by_title failed"
                    );
                }
            }
        }
        let fetch_ms = t_fetch.elapsed().as_millis() as u64;

        tracing::info!(
            sources_expanded,
            chunks_added,
            initial_count = merged.len() - chunks_added,
            final_count = merged.len(),
            top_groups = ?groups
                .iter()
                .map(|(k, _)| format!("{}::{}", k.0, k.1))
                .collect::<Vec<_>>(),
            fetch_ms,
            "multi-source expansion (additive)"
        );

        (merged, sources_expanded, chunks_added)
    }
    /// Retrieve the candidate chunk set the team-pipeline Curator
    /// will reduce — local + mesh search, atlas grounding, entity
    /// boost, optional decomposition, dedupe, reweight, multi-source
    /// expansion. Returns just the chunks; callers that also need
    /// provenance (search-method label, per-corpus source counts,
    /// peer attribution) currently re-call `prepare_knowledge_context`
    /// for the formatted shape.
    ///
    /// This is the Phase 2.5 seam from the situated-team plan
    /// (`/Users/alexsbryan/.claude/plans/there-s-a-fast-slot-delightful-peach.md`).
    /// Implementation today is the minimal wrapper — runs the
    /// existing `prepare_knowledge_context` pipeline and discards
    /// the formatter output. Phase 4 wires this directly into
    /// `run_team_pipeline` and at that point the wrapper gets
    /// expanded into a real split so the wasted formatting work on
    /// the team-pipeline path is paid only once.
    pub(crate) async fn retrieve_candidates(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
    ) -> Vec<corpus_engine::ScoredChunk> {
        let kc = self
            .prepare_knowledge_context(message, context, intent, None)
            .await;
        kc.chunks
    }
    /// Search all knowledge sources, build the prompt with retrieved context,
    /// and assemble provenance metadata. Shared between the streaming and
    /// non-streaming response paths so they cannot diverge.
    pub(crate) async fn prepare_knowledge_context(
        &self,
        message: &str,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> KnowledgeContext {
        // Document-attached messages are detected by the
        // `[Document attached: filename]` prefix. We only need to
        // know whether one is attached — the actual document
        // chunking has been moved to `DocumentOperationTool`
        // (routed via ComplexTask), so the parsed-out filename and
        // query text aren't consumed here. We still detect the
        // prefix to short-circuit the embed/search path; without
        // this, a stray document-attached message would burn an
        // embed call producing useless context.
        let attached_source: Option<String> = message
            .strip_prefix("[Document attached: ")
            .and_then(|rest| rest.find(']').map(|end| rest[..end].to_string()));

        let mut all_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        // corpus_id → human-readable peer name, used at the end to
        // stamp `SourceSummary.from_peer` on any corpus whose hits
        // came in via the mesh. Only populated for corpora we
        // don't host locally (so a corpus present both sides stays
        // tagged as local — we don't pretend to "serve from
        // BeefyMac" a corpus we have right here).
        let mut peer_attribution: HashMap<String, String> = HashMap::new();
        // How many hits came from local (before mesh). Drives the
        // computed `search_method` label. `mesh_hits` is derived
        // later from the peer-attribution map after dedupe.
        let mut local_hits: usize = 0;

        if attached_source.is_some() {
            // Document-attached messages are routed to ComplexTask and should
            // never reach this path — the planner invokes DocumentOperationTool
            // for full map-reduce across all chunks. If we somehow get here,
            // return empty context rather than stuffing a few search results
            // into the prompt.
            tracing::debug!("prepare_knowledge_context called with attached document — skipping (should be ComplexTask)");
        } else {
            // Normal mode: search installed corpora (corpus-engine LanceDB)
            // and corpus-type documents in StateStore. User-uploaded documents
            // are NOT included — they are only surfaced when explicitly
            // attached via [Document attached: ...].
            let retrieval_query = build_retrieval_query(message, context);
            if retrieval_query != message {
                tracing::debug!(
                    bare_chars = message.len(),
                    expanded_chars = retrieval_query.len(),
                    "retrieval: expanded follow-up query with prior user turns"
                );
            }
            let corpus_embedding =
                self.inference.embed_query(&retrieval_query).await.unwrap_or_default();
            let label = format!("{intent:?}");

            // Run the local corpus search and the mesh fan-out
            // concurrently — the mesh call does HTTP (up to ~3s
            // budget per peer), the local call is LanceDB disk I/O,
            // so there's no point serialising them. `tokio::join!`
            // waits for both.
            // K calibration mirrors KnowledgeQuery (`KQ_PER_CORPUS_LIMIT`,
            // `KQ_MERGED_LIMIT`). DeepQuery is the path multi-article
            // synthesis questions take ("How did the Treaty of Versailles
            // contribute to WWII?", "How did Stalin's and Churchill's
            // styles differ?"). At K=5/corpus → top-8, the merged set
            // contained only 1-2 chunks per source article — not enough
            // depth for the model to write a sourced multi-paragraph
            // answer. At K=20/corpus → top-15, the merge holds 4-5
            // articles each with 2-3 chunks: real synthesis material.
            let hot_corpora_dq = collect_hot_corpora(&context.conversation.messages);
            let per_corpus_overrides_dq =
                build_per_corpus_k_overrides(&hot_corpora_dq, KQ_PER_CORPUS_LIMIT);
            let enabled_corpora_dq = context.conversation.enabled_corpora.as_deref();
            let local_corpora_fut = self.search_corpus_indexes_with_overrides(
                &corpus_embedding,
                message,
                KQ_PER_CORPUS_LIMIT,
                &label,
                per_corpus_overrides_dq.as_ref(),
                enabled_corpora_dq,
            );
            let mesh_fut = async {
                match &self.mesh_knowledge {
                    Some(m) => m.search(message, &corpus_embedding, KQ_PER_CORPUS_LIMIT).await,
                    None => Vec::new(),
                }
            };
            let (mut local_scored, mesh_scored) = tokio::join!(local_corpora_fut, mesh_fut);

            // Scope filter: when the router classified this turn as
            // `scope = "personal"`, restrict the local hits to
            // user-owned corpora so the synthesis prompt isn't
            // dominated by general-knowledge sources. Prefix match
            // is a TODO placeholder for recipe-level
            // `[corpus] scope = "personal"` annotation — same
            // pattern as the KQ plan path (see
            // `prepare_knowledge_query_plan`).
            if matches!(scope, Some("personal")) {
                // TODO: replace prefix match with recipe-level
                // `[corpus] scope = "personal"` annotation read from
                // installed_indexes(). Mirror of the KQ plan path
                // (see `prepare_knowledge_query_plan`).
                const PERSONAL_CORPUS_PREFIXES: &[&str] =
                    &["conversations-", "personal-", "journal-", "inner-work-"];
                let before = local_scored.len();
                local_scored.retain(|c| {
                    PERSONAL_CORPUS_PREFIXES
                        .iter()
                        .any(|p| c.corpus_id.starts_with(p))
                });
                tracing::info!(
                    kept = local_scored.len(),
                    dropped = before.saturating_sub(local_scored.len()),
                    scope = ?scope,
                    label = %label,
                    "prepare_knowledge_context: scope-filtered retrieval to personal-corpus prefixes"
                );
            }

            local_hits = local_scored.len();
            // Glass-box log: how many hits from local vs. mesh, and
            // which corpora did mesh claim to serve? If mesh_hits > 0
            // but `peer_tagged` is 0, the mesh is only round-tripping
            // local corpora — meaning no peer actually hosts anything
            // we're missing. If both are 0 with a live mesh, the
            // handler on :9741 is either not running or returning
            // empty. Reading this line is how you tell.
            let peer_tagged = mesh_scored
                .iter()
                .filter(|h| h.peer_name.is_some())
                .count();
            let mesh_corpora: std::collections::BTreeSet<&str> = mesh_scored
                .iter()
                .map(|h| h.corpus_id.as_str())
                .collect();
            tracing::info!(
                local_hits = local_scored.len(),
                mesh_hits = mesh_scored.len(),
                mesh_peer_tagged = peer_tagged,
                mesh_corpora = ?mesh_corpora,
                "runtime: knowledge fan-out summary"
            );
            all_chunks.extend(local_scored);

            // Fold mesh hits in, tagging peer attribution per corpus.
            // A corpus that already appears locally doesn't get
            // tagged — we own it, mesh is just parroting.
            let local_corpora_ids: std::collections::HashSet<String> =
                all_chunks.iter().map(|c| c.corpus_id.clone()).collect();
            for hit in mesh_scored {
                if !local_corpora_ids.contains(&hit.corpus_id) {
                    if let Some(name) = &hit.peer_name {
                        peer_attribution
                            .entry(hit.corpus_id.clone())
                            .or_insert_with(|| name.clone());
                    }
                }
                // Phase C4: stamp peer attribution on the chunk
                // itself so eval --inspect / desktop hit panels can
                // show "peer:<name>" inline. peer_attribution above
                // is corpus-level; metadata is per-chunk.
                let mut metadata = HashMap::new();
                if let Some(name) = &hit.peer_name {
                    metadata.insert("peer".to_string(), name.clone());
                    metadata.insert("source".to_string(), "mesh".to_string());
                }
                all_chunks.push(corpus_engine::ScoredChunk {
                    content: hit.content,
                    title: hit.title,
                    url: hit.url,
                    corpus_id: hit.corpus_id,
                    score: hit.score,
                    metadata,
                    chunk_id: hit.chunk_id,
                    source_doc_id: hit.source_doc_id,
                    // Mesh-served hits don't carry vector_distance
                    // over the wire today; the cross-corpus merge
                    // falls back to score-sort for them.
                    vector_distance: None,
                });
            }

            // Atlas grounding — fuse pre-embedded Entity matches as
            // virtual ScoredChunks (corpus_id = "atlas:<corpus>").
            // Mirror of the KnowledgeQuery path's 2d step
            // (`prepare_knowledge_query_plan`); DeepQuery /
            // ComparisonQuery / contested-style intents take this
            // route and benefit equally from atlas grounding.
            // Same env override (`SOVEREIGN_ATLAS_GROUNDING=0`)
            // applies here.
            self.apply_atlas_grounding(
                message,
                &corpus_embedding,
                &mut all_chunks,
                "DeepQuery",
                scope,
                context.conversation.enabled_corpora.as_deref(),
            )
            .await;

            // Also search StateStore for corpus-type documents (used by test
            // harness and for corpora ingested directly into the store).
            let embedding = self.inference.embed(message).await.unwrap_or_default();
            let store_chunks = self
                .store
                .search_documents(&embedding, message, 5)
                .await
                .unwrap_or_default();
            for doc in &store_chunks {
                // Only include corpus-type documents, not user uploads.
                if matches!(doc.source_type, SourceType::Corpus { .. }) {
                    all_chunks.push(corpus_engine::ScoredChunk {
                        content: doc.content.clone(),
                        title: Some(doc.source.clone()),
                        url: None,
                        corpus_id: match &doc.source_type {
                            SourceType::Corpus { corpus_id } => corpus_id.clone(),
                            _ => "unknown".to_string(),
                        },
                        score: 0.5,
                        metadata: HashMap::new(),
                        chunk_id: None,
                        source_doc_id: None,
                        vector_distance: None,
                    });
                }
            }
        }

        // Entity boost — extract proper-noun entities from the
        // question and run a focused hybrid search per entity. The
        // bag-of-words query embedding tends to land on topic-central
        // articles (e.g. "How do Einstein's and Newton's conceptions
        // of gravity differ?" surfaces "Introduction to general
        // relativity" but not the Albert Einstein and Isaac Newton
        // articles — those are more biographical than thematic for
        // the embedded query). A per-entity search gives each named
        // entity its own retrieval pass; these articles are almost
        // always fact-rich for the question.
        let entities = extract_question_entities(message);
        if !entities.is_empty() {
            let initial_count = all_chunks.len();
            let mut entity_added = 0usize;
            for entity in entities.iter().take(MAX_ENTITY_QUERIES) {
                let entity_emb = self
                    .inference
                    .embed_query(entity)
                    .await
                    .unwrap_or_default();
                let entity_chunks = self
                    .search_corpus_indexes_with_overrides(
                        &entity_emb,
                        entity,
                        ENTITY_QUERY_LIMIT,
                        "EntityBoost",
                        None,
                        context.conversation.enabled_corpora.as_deref(),
                    )
                    .await;
                entity_added += entity_chunks.len();
                all_chunks.extend(entity_chunks);
            }
            tracing::info!(
                entities = ?entities.iter().take(MAX_ENTITY_QUERIES).collect::<Vec<_>>(),
                initial_count,
                entity_added,
                "DeepQuery: entity-boost retrieval"
            );
        }

        // Canonical-entity boost (Move 4) — same pass as the streaming
        // KQ branch. Surfaces the canonical-overview chunk for any
        // famous entity named in the question, anchored to the
        // registry's primary corpus, regardless of cross-corpus cosine
        // ranking. Records are not threaded through KnowledgeContext
        // (the non-streaming surface is unused by the bench), but the
        // chunks they inject still survive the merge below.
        let meta_atlas_hits = self
            .meta_atlas_boost(
                &mut all_chunks,
                &entities,
                context.conversation.enabled_corpora.as_deref(),
            )
            .await;

        // Optional question decomposition (gated by env flag). Catches
        // concept axes that proper-noun extraction misses ("compassion",
        // "indeterminism") and gives each named side of a comparison
        // its own focused retrieval pass.
        if let Some(sub_queries) = self.decompose_question(message, intent) {
            let added = self
                .fan_out_decomposed_queries(
                    &sub_queries,
                    &mut all_chunks,
                    "QueryDecomp",
                    context.conversation.enabled_corpora.as_deref(),
                )
                .await;
            tracing::info!(
                sub_queries = sub_queries.len(),
                chunks_added = added,
                "DeepQuery: query-decomp retrieval"
            );
        }

        // Title expansion (opt-in via SOVEREIGN_TITLE_EXPAND=1).
        // DeepQuery is the path many comparative/contested/synthesis
        // questions take, where abstract phrasings need explicit
        // Wikipedia titles named ("Christopher Columbus", "Buddhism",
        // "Atomic bombings of Hiroshima and Nagasaki") for retrieval
        // to land. Mirrors the KnowledgeQuery wiring.
        let title_expand_titles_dq: Option<Vec<String>> =
            self.expand_question_to_titles(message, context).await;
        if let Some(titles) = &title_expand_titles_dq {
            let added = self
                .fan_out_decomposed_queries(
                    titles,
                    &mut all_chunks,
                    "TitleExpand",
                    context.conversation.enabled_corpora.as_deref(),
                )
                .await;
            tracing::info!(
                titles = ?titles,
                chunks_added = added,
                "DeepQuery: title-expand retrieval"
            );
        }

        // Noise floor — drop chunks with zero query-token overlap in
        // both title and content. These survived hybrid RRF on a weak
        // tangential signal (one shared FTS token in a 1024-char
        // chunk, or vector similarity to phrasing rather than topic);
        // they fill prompt budget the model can't act on. See KQ
        // path comment for the v33 / v36 protection design history.
        let pre_floor = all_chunks.len();
        all_chunks = drop_no_overlap_chunks(all_chunks, message);
        if all_chunks.len() < pre_floor {
            tracing::info!(
                pre_floor,
                post_floor = all_chunks.len(),
                "DeepQuery: noise floor dropped no-overlap chunks"
            );
        }

        // Reweight chunks by query relevance before the global merge.
        // RRF rank-1 chunks across corpora come back at the same raw
        // score (~0.033 with k=60), so without a relevance signal an
        // off-domain corpus's barely-related top hit ties with the
        // canonical Wikipedia article on a Wikipedia-domain question.
        // Reweighting by title- + content-token overlap with the
        // query lets in-domain chunks rise; off-domain chunks stay at
        // their RRF baseline and naturally sink in the truncation.
        reweight_by_query_relevance(&mut all_chunks, message);

        // Dedupe by (corpus_id, content) before truncating so a
        // corpus that appears both locally and via mesh doesn't
        // waste context budget on duplicate chunks.
        all_chunks.sort_by(cross_corpus_sort_cmp);

        // Optional structural-graph one-hop expansion. DeepQuery
        // is *by classifier* always reasoning across sources, so
        // there's no FastFocused/PrimarySynthesis gate here — the
        // helper itself handles the env-flag opt-in.
        if let Some(neighbors) = self
            .expand_via_wikipedia_graph(
                &all_chunks,
                message,
                context.conversation.enabled_corpora.as_deref(),
            )
            .await
        {
            if !neighbors.is_empty() {
                let added = neighbors.len();
                all_chunks.extend(neighbors);
                reweight_by_query_relevance(&mut all_chunks, message);
                all_chunks.sort_by(cross_corpus_sort_cmp);
                tracing::info!(
                    added,
                    total = all_chunks.len(),
                    "DeepQuery: graph one-hop expansion"
                );
            }
        }

        {
            let mut seen: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            all_chunks.retain(|c| seen.insert((c.corpus_id.clone(), c.content.clone())));
        }
        all_chunks = cap_chunks_per_article(all_chunks, MAX_CHUNKS_PER_ARTICLE_AT_MERGE);
        // Title-expand reservation. Mirrors the KnowledgeQuery
        // wiring — pins chunks from title-expand titles before the
        // KQ_MERGED_LIMIT truncate so the multi-source expander below
        // can't displace them by picking a different dominant article.
        if let Some(titles) = &title_expand_titles_dq {
            if !titles.is_empty() {
                all_chunks = reserve_chunks_per_entity(
                    all_chunks,
                    titles,
                    COMPARISON_PER_ENTITY_RESERVE,
                );
            }
        }
        all_chunks.truncate(KQ_MERGED_LIMIT);

        // Multi-source cohesion expansion. DeepQuery is the path
        // multi-article synthesis questions take, so this is exactly
        // where pulling depth from the top-N source documents pays
        // off (see `expand_from_top_sources` for the rationale).
        // Single-source dominance is rare here — DeepQuery questions
        // are by-classifier "REASONING" — but the expander returns
        // initial unchanged when fewer than 2 distinct titled sources
        // appear, so it's safe to call unconditionally.
        let (mut all_chunks, sources_expanded, _total_fetched) =
            self.expand_from_top_sources(all_chunks).await;

        // DeepQuery/Simple glassbox (opt-in). This path has no
        // evidence-shape `turn_summary` (that lives in the KQ planner),
        // so DeepQuery turns — multi_article_synthesis, causal_reasoning,
        // contested — were invisible to the retrieval audit. Emit the
        // FINAL composition (post sort + truncate + top-sources expand +
        // graph one-hop) so cross-corpus dilution is diagnosable here:
        // `final_by_corpus` answers "did the target corpus survive the
        // merge, or did SEP/catalog/fetched crowd it out?" — the thing
        // the pre-merge `merged_pool` event can't show. Gated on the
        // `retrieval_audit` target so production pays only a level-check.
        if tracing::enabled!(target: "retrieval_audit", tracing::Level::INFO) {
            use std::collections::{HashMap, HashSet};
            let mut by_corpus: HashMap<String, usize> = HashMap::new();
            let mut seen: HashSet<String> = HashSet::new();
            for c in &all_chunks {
                *by_corpus.entry(c.corpus_id.clone()).or_insert(0) += 1;
                seen.insert(
                    c.source_doc_id
                        .clone()
                        .or_else(|| c.title.clone())
                        .unwrap_or_default(),
                );
            }
            let mut corpus_pairs: Vec<(String, usize)> = by_corpus.into_iter().collect();
            corpus_pairs.sort_by(|a, b| b.1.cmp(&a.1));
            let query_preview: String = message.chars().take(80).collect();
            tracing::info!(
                target: "retrieval_audit",
                event = "deep_turn_summary",
                intent = ?intent,
                query = %query_preview,
                final_chunks = all_chunks.len(),
                distinct_sources = seen.len(),
                final_by_corpus = ?corpus_pairs,
                sources_expanded,
                meta_atlas_hits = meta_atlas_hits.len(),
                "retrieval_audit: deep_turn_summary"
            );
        }

        // Count mesh hits that survived dedupe so the search_method
        // label reflects what's actually in the prompt.
        let mesh_hits: usize = all_chunks
            .iter()
            .filter(|c| peer_attribution.contains_key(&c.corpus_id))
            .count();

        // 4. Provenance metadata.
        let installed_corpora = self
            .store
            .list_corpus_states()
            .await
            .unwrap_or_default();
        let corpora_searched = !installed_corpora.is_empty() || self.corpus_engine.is_some();

        // Compose a human-readable label that describes *where* the
        // hits came from. This replaces the old hardcoded "LocalOnly"
        // string — the UI surface is unchanged (still a string in
        // `provenance.search_method`), but the content is now
        // truthful.
        let search_method = if all_chunks.is_empty() {
            if self.mesh_knowledge.is_some() {
                if corpora_searched {
                    Some("LocalAndMesh (no matches)".to_string())
                } else {
                    Some("Mesh (no matches)".to_string())
                }
            } else if corpora_searched {
                Some("LocalOnly (no matches)".to_string())
            } else {
                None
            }
        } else if mesh_hits > 0 && local_hits > 0 {
            Some("LocalAndMesh".to_string())
        } else if mesh_hits > 0 {
            Some("MeshOnly".to_string())
        } else {
            Some("LocalOnly".to_string())
        };

        let mut source_map: HashMap<String, usize> = HashMap::new();
        for c in &all_chunks {
            *source_map.entry(c.corpus_id.clone()).or_insert(0) += 1;
        }
        if all_chunks.is_empty() && corpora_searched {
            for cs in &installed_corpora {
                source_map.entry(cs.corpus_id.clone()).or_insert(0);
            }
        }
        let folder_meta_for_ctx = self.folder_metadata_snapshot().await;
        // Build the corpus-kind + display-category lookups before
        // the provenance components so the SourceSummary
        // `display_name` can render "Your conversations" for any
        // corpus declaring `[display] category = "conversation"`.
        // Catalog routing + Wikipedia editors' POV markers —
        // best-effort: `installed_indexes()` errors fall through to
        // defaults, so no callsite gates on the engine being
        // configured.
        let (kinds, display_categories): (
            std::collections::HashMap<String, corpus_engine::CorpusKind>,
            std::collections::HashMap<String, String>,
        ) = if let Some(engine) = &self.corpus_engine {
            let mut kinds_map = std::collections::HashMap::new();
            let mut display_map = std::collections::HashMap::new();
            for info in engine.installed_indexes().await.unwrap_or_default() {
                if let Some(d) = &info.display {
                    if let Some(cat) = &d.category {
                        display_map.insert(info.corpus_id.clone(), cat.clone());
                    }
                }
                kinds_map.insert(info.corpus_id, info.kind);
            }
            (kinds_map, display_map)
        } else {
            Default::default()
        };
        let (sources, coverage) = build_provenance_components(
            &source_map,
            &peer_attribution,
            &folder_meta_for_ctx,
            if display_categories.is_empty() {
                None
            } else {
                Some(&display_categories)
            },
        );

        // 5. Build prompt with knowledge context.
        //
        // Use the EXPANDED budget here because `prepare_knowledge_context`
        // is the DeepQuery path and the multi-source expander above
        // may have appended depth-fetched chunks beyond the initial
        // top-K. The formatter takes chunks in order until the budget
        // is hit; if we kept `MAX_KNOWLEDGE_CHARS` (8000) the appended
        // depth chunks would never reach the prompt — which is the
        // exact failure mode v6 surfaced empirically (chunks_fact_score
        // climbed but answer-fact-score didn't, because the model
        // never saw the depth chunks).
        let contested_titles: std::collections::HashSet<String> =
            self.contested_titles_for_chunks(&all_chunks).await;
        let folder_meta = self.folder_metadata_snapshot().await;

        let history = format_history_as_prompt(context, 10);
        let prompt = if !all_chunks.is_empty() {
            // Conv-tiered briefing — surface per-conversation RAPTOR
            // signposts ahead of the raw chunks when retrieval hit a
            // conversation corpus. No-op when no reader wired or no
            // conv-category chunks present. Spec
            // `sovereign/docs/specs/CONV_TIERED_PORT.md`.
            self.rerank_conv_chunks_via_ppr(message, &mut all_chunks, &display_categories)
                .await;
            let conv_briefing = self.build_conv_briefing_block(
                &all_chunks,
                &display_categories,
            ).await;
            let doc_context = format_scored_chunks_with_kinds(
                &all_chunks,
                EXPANDED_KNOWLEDGE_CHARS,
                Some(&kinds),
                if contested_titles.is_empty() {
                    None
                } else {
                    Some(&contested_titles)
                },
                if folder_meta.is_empty() {
                    None
                } else {
                    Some(&folder_meta)
                },
                if display_categories.is_empty() {
                    None
                } else {
                    Some(&display_categories)
                },
            );
            let knowledge_block = if conv_briefing.is_empty() {
                doc_context
            } else {
                format!("{conv_briefing}\n{doc_context}")
            };
            if history.is_empty() {
                format!(
                    "Relevant knowledge:\n{knowledge_block}\n\nUser: {message}\n\nAssistant:"
                )
            } else {
                let short_history = format_history_as_prompt(context, 4);
                format!(
                    "{short_history}\n\nRelevant knowledge:\n{knowledge_block}\n\nAssistant:"
                )
            }
        } else if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        // 6. System message — layered confidence when knowledge is present.
        // Folder-ingest v1 §6.3: when a watched-folder corpus
        // contributed retrieval AND carries non-zero
        // failed_files/skipped_by_extension, append a one-line
        // "what I don't have" note so the synthesis is honest
        // about the user's coverage gap. Empty string when no
        // gaps — adds zero prompt overhead.
        let gap_note = build_coverage_gaps_note(&all_chunks, &folder_meta_for_ctx);
        // Budget reminder — same directive spliced into the
        // KnowledgeQuery synthesis routes. Tells the model how much
        // room it has so it picks a shape that lands within the
        // budget instead of opening a multi-section essay that gets
        // cut off mid-paragraph (the bug the cutoff chip surfaces on
        // the desktop side).
        let budget_note = crate::runtime::build_response_length_directive(
            self.inference_config.max_tokens,
        );
        let system = if !all_chunks.is_empty() {
            let base = if gap_note.is_empty() {
                format!(
                    "{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{THINKING_DIRECTIVE}\n\n{budget_note}"
                )
            } else {
                format!(
                    "{KNOWLEDGE_SYNTHESIS_SYSTEM}\n\n{gap_note}\n\n{THINKING_DIRECTIVE}\n\n{budget_note}"
                )
            };
            self.build_primary_system_message(&base, context)
        } else {
            self.build_system_message(
                &format!(
                    "You are a helpful AI assistant. Respond concisely and accurately.\n\n{budget_note}"
                ),
                context,
            )
        };

        // 7. Speed upgrade: if knowledge found for SimpleQuery, use Slow.
        let speed = match intent {
            Intent::SimpleQuery => {
                if !all_chunks.is_empty() {
                    Speed::Slow
                } else {
                    Speed::Fast
                }
            }
            Intent::DeepQuery => Speed::Slow,
            // Bounded contrast — Fast slot is enough; the constrained
            // synthesis prompt does the structuring work the primary
            // model would otherwise do.
            Intent::ComparisonQuery => Speed::Fast,
            _ => Speed::Medium,
        };

        // 8. Build chunk summaries for frontend source linking.
        // chunk_id and source_doc_id are emitted (when present) so the
        // desktop reading surface can deref a citation back to the
        // source chunk for in-app reading + atom-graph overlay.
        let retrieved_chunks: Vec<serde_json::Value> = all_chunks
            .iter()
            .map(|c| {
                let snippet = truncate_with_ellipsis(&c.content, 200);
                // Conv-tiered PPR provenance (A3-lite) — emit the
                // metadata map so the desktop SourceAttribution
                // component can render an "↗ surfaced via entity
                // bridge" subtitle on chunks the entity graph
                // boosted. Frontend gates on
                // `metadata.ppr_mass_norm > 0.5`.
                serde_json::json!({
                    "title": c.title.as_deref().unwrap_or(""),
                    "corpus_id": c.corpus_id,
                    "url": c.url,
                    "snippet": snippet,
                    "provenance_tier": if c.url.is_some() { "web" } else { "corpus" },
                    "chunk_id": c.chunk_id,
                    "source_doc_id": c.source_doc_id,
                    "metadata": c.metadata,
                })
            })
            .collect();

        KnowledgeContext {
            chunks: all_chunks,
            prompt,
            system,
            speed,
            search_method,
            sources,
            retrieved_chunks,
            coverage,
        }
    }
    /// Summarize the dropped tail of the conversation so the
    /// synthesis prompt retains an anchor for entities and topics
    /// established outside the visible-history window.
    ///
    /// Activates only when `conversation.messages.len()` exceeds the
    /// visible-history window by at least
    /// `CONV_HISTORY_COMPACT_MIN_DROPPED` messages. The summary is
    /// stored on `context.compacted_history` and read back by
    /// `format_conversation_history` at prompt-assembly time.
    ///
    /// Soft-fail by design: a parse failure or an inference error
    /// leaves `compacted_history = None` and the synthesis path
    /// continues on just the visible window. Surfaced by
    /// `sovereign/bench/wikipedia_learn` 2026-05-17 marathon thread.
    pub(crate) async fn maybe_compact_dropped_history(
        &self,
        context: &mut ConversationContext,
        conversation_id: &str,
        // Optional because the compaction call fires earlier in the
        // streaming handler (line ~1355) than `self.sessions.begin`
        // (line ~1432), so the session_id isn't bound yet on the
        // critical path. Non-streaming and test callers don't have
        // a session at all. Emit the narration chip only when
        // Some; below that we still fire compaction + the
        // `runtime:compaction.budget_triggered` trace, just no chip.
        session_id: Option<&str>,
    ) {
        // v5 spike (2026-05-26): when retrieval-over-history is the
        // primary memory mechanism for old turns, the lossy-summary
        // compaction arm fights it (adds a re-summarised preamble
        // that competes with the retrieval block). Env-var off lets
        // bench A/B the two cleanly.
        if std::env::var("SOVEREIGN_COMPACTION_DISABLE").ok().as_deref() == Some("1") {
            tracing::debug!(conversation_id, "runtime:compaction.disabled_via_env");
            let _ = session_id;
            return;
        }
        let total = context.conversation.messages.len();
        // Two-axis trigger (added 2026-05-25 in the
        // marathon-graceful pass):
        //   1. **Turn-count arm** (original): visible window has
        //      already overflowed, oldest messages are about to be
        //      dropped silently. This is the steady-state trigger on
        //      typical multi-turn chats.
        //   2. **Budget-pressure arm** (new): the conversation
        //      already exceeds `COMPACTION_PRESSURE_THRESHOLD * ctx`
        //      even with all turns visible. Catches the case where 6
        //      verbose turns on a tight slot would blow ctx before
        //      the turn-count arm fires.
        let turn_count_trigger = total > CONV_HISTORY_TURNS;
        let (budget_trigger, budget_pressure, budget_ctx) =
            self.estimate_compaction_pressure(context);
        if !turn_count_trigger && !budget_trigger {
            return;
        }

        // Pick the dropped window. Turn-count arm keeps its existing
        // shape (everything before `last 8`). Budget arm without the
        // turn-count arm drops just the oldest pair so the chat
        // shrinks one user/assistant pair at a time as pressure
        // climbs — leaves the recent context maximally intact.
        let dropped_end = if turn_count_trigger {
            total.saturating_sub(CONV_HISTORY_TURNS)
        } else {
            // Budget-only arm. Need ≥ 4 messages to drop a pair
            // without leaving the visible window degenerate.
            if total < 4 {
                return;
            }
            2
        };
        let dropped = &context.conversation.messages[..dropped_end];
        if dropped.len() < CONV_HISTORY_COMPACT_MIN_DROPPED {
            return;
        }

        if budget_trigger {
            tracing::debug!(
                turn_count_trigger,
                budget_trigger,
                budget_pressure,
                budget_ctx,
                dropped = dropped.len(),
                total,
                "runtime:compaction.budget_triggered"
            );
        }

        match crate::context::summarize_dropped_history(
            self.inference.as_ref(),
            dropped,
        )
        .await
        {
            Ok(summary @ Some(_)) => {
                context.compacted_history = summary;
                // Glassbox the compaction so the user sees why their
                // chat surface changed shape. Gated below
                // `COMPACTION_CHIP_MIN_DROPPED = 3` — folding 2
                // messages would chip-spam on every long-chat turn.
                let dropped_count = dropped.len();
                if dropped_count >= crate::runtime::COMPACTION_CHIP_MIN_DROPPED {
                    if let Some(sid) = session_id {
                        self.routing_events
                            .emit_turn_narration(crate::types::TurnNarration {
                                session_id: sid.to_string(),
                                conversation_id: conversation_id.to_string(),
                                event: crate::types::NarrationEvent {
                                    phase: crate::types::NarrationPhase::GapCheckFired,
                                    text: format!(
                                        "Folded {dropped_count} earlier turns into a summary to keep context fresh."
                                    ),
                                    elapsed_ms: 0,
                                },
                            })
                            .await;
                    }
                }
            }
            Ok(None) => {
                tracing::debug!(
                    dropped = dropped.len(),
                    "context: summarize_dropped_history returned None — falling back to visible window only"
                );
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dropped = dropped.len(),
                    "context: summarize_dropped_history failed — falling back to visible window only"
                );
            }
        }
    }

    /// Estimate the conversation-history-side pressure on the slot's
    /// context window. Returns `(triggered, estimated_tokens,
    /// ctx_size)`. `triggered` is true iff the estimate crosses
    /// `COMPACTION_PRESSURE_THRESHOLD * ctx_size`. Returns
    /// `(false, 0, 0)` when the inference provider doesn't expose a
    /// concrete context window (remote-only forwarder) — the
    /// turn-count arm carries the trigger in that case.
    ///
    /// **NARROW SENSOR — KNOWN LIMITATION.** Walks only the
    /// components the runtime knows ABOUT BEFORE RETRIEVAL fires:
    ///   * visible conversation history (per-msg capped to
    ///     `CONV_HISTORY_CHARS_PER_MSG`),
    ///   * the compacted preamble we've already emitted on a prior
    ///     turn (if any — saves the call when the slot was already
    ///     hot),
    ///   * recalled memories (top-K, bounded).
    ///
    /// System message (persona + epistemic contract + thinking
    /// directive + tool dossier) and retrieval bundle are NOT
    /// measured — both fire later in the handler. The split is
    /// deliberate (compaction decides before retrieval runs and must
    /// not depend on retrieval state) but it makes this sensor
    /// systematically under-count when the system+retrieval terms
    /// are the dominant pressure source.
    ///
    /// Bench result (marathon_graceful 2026-05-26, three trials at
    /// PRESSURE_THRESHOLD ∈ {0.55, 0.7}): tuning the threshold
    /// against this narrow sensor monotonically regressed
    /// paraphrase-judge coverage (0.764 → 0.694 → 0.639). The
    /// thresholds that fire often enough to matter were firing
    /// when full-prompt was actually fine, triggering wasteful
    /// Fast-slot summarisation that lossy-compressed the preamble
    /// across multiple invocations. PRESSURE_THRESHOLD reverted to
    /// 0.9 (effective emergency-only); the architectural fix is a
    /// full-prompt sensor that takes (system_estimate,
    /// retrieval_estimate, history_estimate, response_reserve) —
    /// captured as a kind=todo note for the next iteration cycle.
    fn estimate_compaction_pressure(
        &self,
        context: &ConversationContext,
    ) -> (bool, u32, u32) {
        let Some(ctx_size) = self.inference.effective_context_size() else {
            return (false, 0, 0);
        };
        let threshold = (ctx_size as f32 * crate::runtime::COMPACTION_PRESSURE_THRESHOLD) as u32;

        let mut total: u32 = 0;
        // Visible conversation history: same per-msg truncate the
        // formatter applies. Use `count_tokens` on the truncated
        // body, not the full body — over-counting here would fire
        // compaction too aggressively.
        for msg in context.conversation.messages.iter() {
            let raw = &msg.content;
            let mut end = raw.len().min(CONV_HISTORY_CHARS_PER_MSG);
            while end > 0 && !raw.is_char_boundary(end) {
                end -= 1;
            }
            total = total.saturating_add(self.inference.count_tokens(&raw[..end]));
        }
        // Pre-existing compacted preamble (from a prior turn on this
        // conversation). It rides every prompt until the slot
        // unloads.
        if let Some(s) = &context.compacted_history {
            total = total.saturating_add(self.inference.count_tokens(s));
        }
        // Recalled memories — bounded at the FTS top-K but each can
        // carry 100-500 tokens.
        for mem in &context.memories {
            total = total.saturating_add(self.inference.count_tokens(&mem.content));
        }

        (total > threshold, total, ctx_size)
    }

    /// Retrieval-over-history spike (2026-05-26).
    ///
    /// Replaces — at least on the callback workload that crushed
    /// marathon_graceful T17-T20 — the lossy-summary mechanism with
    /// embedding-similarity retrieval over prior turns.
    ///
    /// Mechanism: embed each user+assistant pair *outside* the visible
    /// window, embed the current user message, cosine top-K (K=3),
    /// stash the hits on `context.history_retrieval_hits`. The renderer
    /// in `build_system_message` formats them as a "Relevant earlier
    /// turns:" prompt section.
    ///
    /// Gated on `SOVEREIGN_HISTORY_RETRIEVAL=1` for the spike phase.
    /// Off → no-op. On → runs after `maybe_compact_dropped_history`
    /// so the two can coexist during the A/B (the renderer will show
    /// both blocks if both fire — bench tells us which carries weight).
    ///
    /// Soft-fail by design: embed errors leave hits = None and the
    /// synthesis path continues on the existing compacted preamble +
    /// visible window.
    pub(crate) async fn maybe_retrieve_relevant_history(
        &self,
        context: &mut ConversationContext,
        user_message: &str,
    ) {
        // Default-on as of 2026-05-26 marathon_graceful spike outcome.
        // `SOVEREIGN_HISTORY_RETRIEVAL=0` disables for A/B compares.
        if std::env::var("SOVEREIGN_HISTORY_RETRIEVAL").ok().as_deref() == Some("0") {
            return;
        }
        tracing::debug!(
            messages_len = context.conversation.messages.len(),
            "runtime:history_retrieval.entry"
        );
        let messages = &context.conversation.messages;
        // Need at least one pair OLDER than the visible window. Visible
        // window is CONV_HISTORY_TURNS most recent messages. The
        // current user message is already pushed (runtime.rs:1386)
        // so subtract 1.
        if messages.len() <= crate::runtime::CONV_HISTORY_TURNS + 1 {
            return;
        }
        let dropped_end = messages.len().saturating_sub(crate::runtime::CONV_HISTORY_TURNS + 1);
        let dropped = &messages[..dropped_end];

        // Build pair-shaped indexable units. Walk in (user, assistant)
        // pairs so each unit carries the question + its answer. Lone
        // trailing user message (if any) gets indexed alone.
        let mut units: Vec<(usize, String)> = Vec::new();
        let mut i = 0;
        while i < dropped.len() {
            let lead = &dropped[i];
            let body = if i + 1 < dropped.len() {
                let follow = &dropped[i + 1];
                format!(
                    "[{:?}] {}\n[{:?}] {}",
                    lead.role,
                    truncate_with_ellipsis(&lead.content, 600),
                    follow.role,
                    truncate_with_ellipsis(&follow.content, 600),
                )
            } else {
                format!("[{:?}] {}", lead.role, truncate_with_ellipsis(&lead.content, 600))
            };
            units.push((i, body));
            i += 2;
        }
        if units.is_empty() {
            return;
        }

        // Embed the candidate units in a single batch + the query
        // separately. embed_batch falls back to per-unit embed on
        // providers that don't override it.
        let unit_texts: Vec<String> = units.iter().map(|(_, b)| b.clone()).collect();
        let unit_embeds = match self.inference.embed_batch(&unit_texts).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, units = units.len(),
                    "runtime:history_retrieval.embed_batch_failed");
                return;
            }
        };
        // Query enrichment (v5 tune): when the runtime extracted a
        // topic_context for this turn, append the topic + domain to
        // the embed-query text. Captures "switching back to <topic>"
        // semantics that bare follow-up phrasing misses (e.g.
        // T19 "And Linnaeus's framework — what part of his work
        // proved least durable?" embeds toward generic biology
        // unless we ride the topic_context anchor).
        let mut query_text = user_message.to_string();
        if let Some(tc) = context.topic_context.as_ref() {
            if let Some(t) = tc.topic.as_ref() {
                query_text.push_str("\n[topic: ");
                query_text.push_str(t);
                query_text.push(']');
            }
            if let Some(d) = tc.domain.as_ref() {
                query_text.push_str("\n[domain: ");
                query_text.push_str(d);
                query_text.push(']');
            }
        }
        let query_embed = match self.inference.embed_query(&query_text).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e,
                    "runtime:history_retrieval.embed_query_failed");
                return;
            }
        };

        // Cosine score. embed/embed_query already normalize, but defend
        // against unnormalized outputs from custom providers.
        let normalize = |v: &Vec<f32>| -> Vec<f32> {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
            v.iter().map(|x| x / n).collect()
        };
        let q_norm = normalize(&query_embed);

        // v7 entity-aware retrieval. When the runtime has a GLiNER
        // extractor wired, extract entities from the query (user
        // message + topic_context) and from each candidate pair, then
        // hybrid-score: 0.6·cosine + 0.4·jaccard. Fixes the v6 T17
        // failure mode where abstract callbacks ("church-and-science
        // theme") cosine-matched the wrong topic. GLiNER unavailable
        // → behaves exactly like v6 (pure cosine + MMR).
        const HYBRID_COSINE_WEIGHT: f32 = 0.6;
        const HYBRID_JACCARD_WEIGHT: f32 = 0.4;
        let query_entities: std::collections::HashSet<String> = if let Some(g) = self.gliner.as_ref() {
            g.extract_entities(&query_text).into_iter().collect()
        } else {
            std::collections::HashSet::new()
        };
        if !query_entities.is_empty() {
            tracing::debug!(
                entities = ?query_entities,
                "runtime:history_retrieval.query_entities"
            );
        }

        let jaccard = |a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>| -> f32 {
            if a.is_empty() || b.is_empty() {
                return 0.0;
            }
            let inter = a.intersection(b).count() as f32;
            let union = a.union(b).count() as f32;
            if union == 0.0 { 0.0 } else { inter / union }
        };

        let gliner = self.gliner.clone();
        let scored: Vec<(usize, String, f32, Vec<f32>)> = unit_embeds
            .into_iter()
            .zip(units.into_iter())
            .map(|(emb, (idx, body))| {
                let e_norm = normalize(&emb);
                let cos: f32 = e_norm.iter().zip(q_norm.iter()).map(|(a, b)| a * b).sum();
                let sim = if let Some(g) = gliner.as_ref() {
                    let pair_ents: std::collections::HashSet<String> =
                        g.extract_entities(&body).into_iter().collect();
                    let j = jaccard(&query_entities, &pair_ents);
                    HYBRID_COSINE_WEIGHT * cos + HYBRID_JACCARD_WEIGHT * j
                } else {
                    cos
                };
                (idx, body, sim, e_norm)
            })
            .collect();

        // v6 tune: MMR (Maximal Marginal Relevance) selection.
        // v5 single trial regressed T20 -0.75 — cosine top-K picks
        // most-similar candidates, which on a "compare across Curie /
        // Linnaeus / Galileo" synthesis turn collapses onto whichever
        // topic dominates the topic_context (one bucket wins, two
        // missed). MMR optimises top-K = argmax λ·sim(d,q) −
        // (1−λ)·max sim(d, selected). λ=0.5 = balanced
        // relevance-vs-diversity. K stays 5, floor stays 0.30.
        const HISTORY_RETRIEVAL_TOP_K: usize = 5;
        const HISTORY_RETRIEVAL_SIM_FLOOR: f32 = 0.30;
        const HISTORY_RETRIEVAL_MMR_LAMBDA: f32 = 0.5;

        let mut candidates: Vec<(usize, String, f32, Vec<f32>)> =
            scored.into_iter().filter(|(_, _, s, _)| *s >= HISTORY_RETRIEVAL_SIM_FLOOR).collect();
        // Sort once descending by relevance for stable MMR seeding.
        candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        let mut selected: Vec<(usize, String, f32)> = Vec::with_capacity(HISTORY_RETRIEVAL_TOP_K);
        let mut selected_embeds: Vec<Vec<f32>> = Vec::with_capacity(HISTORY_RETRIEVAL_TOP_K);

        while selected.len() < HISTORY_RETRIEVAL_TOP_K && !candidates.is_empty() {
            let mut best_pos = 0;
            let mut best_score = f32::MIN;
            for (i, c) in candidates.iter().enumerate() {
                let max_sim_to_selected: f32 = selected_embeds
                    .iter()
                    .map(|s| s.iter().zip(c.3.iter()).map(|(a, b)| a * b).sum::<f32>())
                    .fold(0.0_f32, f32::max);
                let mmr = HISTORY_RETRIEVAL_MMR_LAMBDA * c.2
                    - (1.0 - HISTORY_RETRIEVAL_MMR_LAMBDA) * max_sim_to_selected;
                if mmr > best_score {
                    best_score = mmr;
                    best_pos = i;
                }
            }
            let (idx, body, sim, emb) = candidates.remove(best_pos);
            selected.push((idx, body, sim));
            selected_embeds.push(emb);
        }

        let hits: Vec<crate::types::HistoryRetrievalHit> = selected
            .into_iter()
            .map(|(turn_index, content, similarity)| crate::types::HistoryRetrievalHit {
                turn_index,
                content,
                similarity,
            })
            .collect();

        if hits.is_empty() {
            tracing::debug!(
                candidates = dropped.len() / 2,
                "runtime:history_retrieval.no_hits_above_floor"
            );
            return;
        }
        // Glassbox per-hit summary at debug. Captures the picks chosen
        // by hybrid (cosine·0.6 + jaccard·0.4) + MMR for post-mortem
        // analysis of "did retrieval surface the right earlier turn?"
        // RUST_LOG=sovereign_core::runtime::retrieval=debug to see it.
        let hit_summary: Vec<String> = hits
            .iter()
            .map(|h| format!("T{}@{:.2}", h.turn_index, h.similarity))
            .collect();
        tracing::debug!(
            hits = hits.len(),
            top_sim = hits[0].similarity,
            picked = %hit_summary.join(","),
            "runtime:history_retrieval.populated"
        );
        context.history_retrieval_hits = Some(hits);
    }

    /// Gather the union of `ev-Tn-NNNN` handles emitted by prior
    /// `tool_decision` writes on this conversation, for sampler-side
    /// citation constraint (Tier 2 of tool-framework expansion).
    /// Returns `None` when the NoteStore isn't wired (CLI / test
    /// paths) or no prior decisions carried evidence ids — the
    /// caller's CompletionRequest stays unconstrained on the
    /// citation axis (Tier 1 prompt discipline is the only safety
    /// net on those turns).
    pub(crate) async fn gather_evidence_id_allowlist(
        &self,
        conversation_id: &str,
    ) -> Option<Vec<String>> {
        let notes = self.note_store.as_ref()?;
        let payloads =
            crate::memory::read_recent_tool_decisions(notes, Some(conversation_id), 32)
                .await
                .ok()?;
        let mut ids: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for p in payloads {
            for id in p.evidence_ids {
                if seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            None
        } else {
            tracing::debug!(
                conversation_id,
                ev_id_count = ids.len(),
                "runtime: gathered evidence_id_allowlist from tool_decisions"
            );
            Some(ids)
        }
    }
    /// Tool-Mastery Layer 2 pre-pass. Computes the tool dossier
    /// (tools available + outcome history + ambient state) and
    /// stashes it on `context.tool_dossier` so
    /// `build_system_message` can splice it. Soft-fails: on any
    /// error or a relational skill the field stays `None` and the
    /// splice is a no-op — preserving today's behaviour for
    /// inner-work and for CLI/test harnesses that don't wire a
    /// NoteStore.
    pub(crate) async fn maybe_compute_tool_dossier(
        &self,
        context: &mut ConversationContext,
        conversation_id: &str,
    ) {
        let active_skill_id = self.skills.primary_skill_id_for_conversation();
        let active_skill = active_skill_id
            .as_deref()
            .and_then(|id| self.skills.skill_by_id(id))
            .cloned();
        if let Some(dossier) = crate::dossier::compute_tool_dossier(
            &self.tools,
            self.note_store.as_deref(),
            active_skill.as_ref(),
            Some(conversation_id),
        )
        .await
        {
            tracing::info!(
                conversation_id,
                skill = active_skill.as_ref().map(|s| s.id.as_str()),
                tools = dossier.tools_available.len(),
                outcomes = dossier.outcome_history.len(),
                has_note_store = self.note_store.is_some(),
                "dossier:computed_for_turn"
            );
            context.tool_dossier = Some(dossier);
        } else {
            tracing::info!(
                conversation_id,
                skill = active_skill.as_ref().map(|s| s.id.as_str()),
                has_note_store = self.note_store.is_some(),
                "dossier:skipped_for_turn"
            );
        }
    }
    pub(crate) async fn maybe_splice_temporal_tensions(
        &self,
        context: &mut ConversationContext,
        user_message: &str,
    ) {
        if context.turn_register() != SkillRegister::Relational {
            return;
        }
        // Skip when there's nothing to compare against — common case
        // for casual chat under a relational skill (zero memories
        // retrieved by FTS), zero inference cost.
        if context.memories.is_empty() {
            return;
        }
        match memory::detect_temporal_tensions(
            self.inference.as_ref(),
            user_message,
            &context.memories,
        )
        .await
        {
            Ok(tensions) => {
                if !tensions.is_empty() {
                    tracing::debug!(
                        count = tensions.len(),
                        "runtime: temporal-tension pre-pass surfaced {} cue(s)",
                        tensions.len(),
                    );
                }
                context.temporal_tensions = tensions;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "runtime: temporal-tension pre-pass failed; continuing without",
                );
            }
        }
    }
}

/// Apply the per-conversation corpus allow-list to a pool of
/// `IndexInfo`. Each index passes when its `corpus_id` is in the
/// allow-list OR its `parent_corpus_id` is. The parent-aware branch
/// is what lets layer/satellite corpora (e.g. wikipedia-newsworthy
/// under wikipedia) follow their parent's enabled state without the
/// caller knowing the layer hierarchy. `None` is the no-filter
/// signal — every index passes, bit-identical to pre-feature
/// behavior.
fn apply_corpus_allow_list(
    indexes: Vec<corpus_engine::IndexInfo>,
    allow: Option<&[String]>,
) -> Vec<corpus_engine::IndexInfo> {
    let Some(allow) = allow else {
        return indexes;
    };
    let allow_set: std::collections::HashSet<&str> =
        allow.iter().map(String::as_str).collect();
    indexes
        .into_iter()
        .filter(|info| {
            allow_set.contains(info.corpus_id.as_str())
                || info
                    .parent_corpus_id
                    .as_deref()
                    .map(|p| allow_set.contains(p))
                    .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod allow_list_tests {
    use super::apply_corpus_allow_list;

    fn idx(id: &str, parent: Option<&str>) -> corpus_engine::IndexInfo {
        corpus_engine::IndexInfo {
            corpus_id: id.to_string(),
            corpus_name: id.to_string(),
            path: std::path::PathBuf::new(),
            chunk_count: 0,
            index_size_bytes: 0,
            created_at: 0,
            last_updated: 0,
            embedding_model: String::new(),
            embedding_dimensions: 0,
            mesh_sharing: false,
            query_sharing: false,
            is_shard: false,
            chunk_range: None,
            chunks_expected: None,
            resume_from: None,
            enrichment_enabled: false,
            enriched_chunks: None,
            source_version: None,
            update_manifest_url: None,
            kind: corpus_engine::CorpusKind::Knowledge,
            parent_corpus_id: parent.map(String::from),
            vector_index_built: true,
            canonical_fingerprint: None,
            total_shards: None,
            processed_shards: Vec::new(),
            mutable_merge: None,
            stream: None,
            display: None,
        }
    }

    #[test]
    fn none_passes_everything() {
        let pool = vec![idx("wikipedia", None), idx("sep", None)];
        let out = apply_corpus_allow_list(pool.clone(), None);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn allow_list_filters_to_subset() {
        let pool = vec![idx("wikipedia", None), idx("sep", None), idx("gutenberg", None)];
        let allow = vec!["sep".to_string()];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].corpus_id, "sep");
    }

    #[test]
    fn parent_pulls_in_layers() {
        let pool = vec![
            idx("wikipedia", None),
            idx("wikipedia-newsworthy", Some("wikipedia")),
            idx("sep", None),
        ];
        let allow = vec!["wikipedia".to_string()];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        let ids: Vec<_> = out.iter().map(|i| i.corpus_id.as_str()).collect();
        assert_eq!(ids, vec!["wikipedia", "wikipedia-newsworthy"]);
    }

    #[test]
    fn empty_allow_filters_everything() {
        let pool = vec![idx("wikipedia", None), idx("sep", None)];
        let allow: Vec<String> = vec![];
        let out = apply_corpus_allow_list(pool, Some(&allow));
        assert!(out.is_empty());
    }
}
