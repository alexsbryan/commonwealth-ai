// SPDX-License-Identifier: AGPL-3.0-or-later
//! Query-side expansion: axis-aware Wikipedia-graph neighbor
//! expansion, heuristic question decomposition + sub-query
//! fan-out, and Fast-slot title expansion.

use super::super::*;

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
