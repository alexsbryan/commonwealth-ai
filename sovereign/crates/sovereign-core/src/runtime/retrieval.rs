// SPDX-License-Identifier: AGPL-3.0-or-later
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
use std::sync::Arc;

use crate::traits::*;

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
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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

    /// Entity-typed atom enumeration for enumeration-class questions.
    ///
    /// The failure mode this targets generalizes well beyond any one
    /// corpus: a question that asks for a *set* of same-typed entities
    /// the user never names —
    ///   "which energy companies were counterparties"   → institution
    ///   "who were the executives involved"             → person
    ///   "what themes recur across these essays"        → concept
    /// — embeds into a single query vector that collapses onto the ONE
    /// dominant member of the set. (Measured on the Enron mail: a
    /// counterparty question retrieved 15/28 Dynegy chunks and zero of
    /// the five other companies the answer needs — though Williams
    /// alone carries 128 atoms in-corpus. The facts are present; the
    /// query has no handle on them.) LLM query-expansion
    /// (`expand_question_to_titles`) cannot rescue this: it can only
    /// name entities the question already implies, and an enumeration
    /// question names none. The set the user wants *is* the corpus's
    /// own typed atom graph — so enumerate it directly.
    ///
    /// Two corpus-agnostic stages:
    ///   1. One Fast-slot classify call: enumeration or lookup, and if
    ///      enumeration, over which `EntityType`. Biased to LOOKUP —
    ///      enumeration is the marked, higher-bar case — so an
    ///      already-focused lookup is never polluted with atom noise.
    ///      That pollution (firing on non-enumeration questions) is the
    ///      exact regression that sank the first atom-grounding attempt;
    ///      the gate + the lookup bias are the structural fix.
    ///   2. Rank the `Entity` atoms of that type by GRAPH CENTRALITY
    ///      (edge degree) and take the top-K, one focused sub-query per
    ///      atom name. Degree is the prominence signal that actually
    ///      discriminates: this corpus's `salience` is a flat 0.70
    ///      default (no signal) and post-reconciliation every name is
    ///      frequency-1, but edge degree separates the real cast (Enron
    ///      1096, Lay 923, Dynegy 59) from address-book noise (~0), and
    ///      centrality generalizes across atlas corpora. Atoms are read
    ///      from the in-memory atlas GRAPH — *not* the role-filtered
    ///      context bag. The graph (`AtlasGraph::load_from_disk`) holds
    ///      every atom unconditionally; the embed bag drops no-role
    ///      institutions, which is why the earlier attempt could not see
    ///      El Paso / Calpine. Ranking by degree (never *filtering* on
    ///      role) keeps those no-role-but-real entities in play.
    ///
    /// The sub-queries are fanned out + decayed through the shared
    /// `fan_out_decomposed_queries` helper, identical to title-expand,
    /// so they augment rather than displace strong base hits.
    ///
    /// Opt-in via `SOVEREIGN_ATOM_ENUM=1` (off by default; un-gating
    /// needs the cross-corpus validation TITLE_EXPAND got). Top-K via
    /// `SOVEREIGN_ATOM_ENUM_TOPK` (default 16).
    ///
    /// Returns `None` when: the gate is off, no atlas provider is
    /// attached, the classify call fails or parses empty, the model
    /// says lookup, or the enabled corpora hold no atoms of the chosen
    /// type. Caller proceeds without enumeration in every case.
    pub(crate) async fn enumerate_typed_atom_chunks(
        &self,
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        let atom_enum_on = std::env::var("SOVEREIGN_ATOM_ENUM").ok().as_deref() == Some("1");
        // Default ON (parity push — surface atlas Claims for overview questions
        // in desktop + bench). Set SOVEREIGN_ATOM_ENUM_OVERVIEW=0 to disable.
        let overview_on =
            std::env::var("SOVEREIGN_ATOM_ENUM_OVERVIEW").ok().as_deref() != Some("0");
        if !atom_enum_on && !overview_on {
            return None;
        }
        // Overview/summary path (default ON; SOVEREIGN_ATOM_ENUM_OVERVIEW=0 disables).
        // An overview question ("what is the most important thing in X", "give
        // me an overview / summary of …") names no entity to enumerate, so the
        // entity classifier below would (correctly) decline — and the answer
        // then abstains or confabulates over a diffuse, anchorless chunk pool.
        // But the corpus's atlas Claim atoms ARE its key points; inject them as
        // grounding so the answer is built from the corpus's own assertions.
        // Detected by question shape (no LLM call); returns before the
        // enumerate classify.
        if overview_on && Self::looks_like_overview(message) {
            return self
                .enumerate_overview_claim_chunks(message, enabled_corpora, corpus_ceiling)
                .await;
        }
        if !atom_enum_on {
            return None;
        }
        // Need the atlas graph to enumerate against; bail before the
        // classify call if no provider is attached — otherwise we would
        // pay an LLM round-trip only to find nothing to enumerate.
        let provider = self.atlas_context_provider.as_ref()?;

        // ---- Stage 1: classify enumeration vs lookup (+ target type).
        // Question-shape only, no conversation context: whether a
        // question enumerates a set is a property of its phrasing, not
        // the dialogue around it, and a tight prompt keeps the Fast
        // call fast. Examples are deliberately domain-neutral so the
        // classifier learns the enumerate/lookup distinction, not this
        // corpus's vocabulary.
        let prompt = format!(
            "Classify the question on ONE axis: ENUMERATE or LOOKUP.\n\n\
             ENUMERATE — its core ask is for MULTIPLE same-typed entities (a \
             LIST of several) that the question does NOT name — it asks \
             WHICH or WHO without naming the members, expecting the set as \
             the answer. The requested category must be PLURAL: people, \
             companies / organizations, places, concepts, works. A trailing \
             descriptive clause (\"… and what each did\", \"… and how they \
             relate\") does NOT change this; the core ask is still the set, \
             so it is still ENUMERATE.\n\
             - \"which organizations were involved\" -> enumerate / institution\n\
             - \"who were the members, and what did each contribute\" -> enumerate / person\n\
             - \"what concepts do these texts discuss\" -> enumerate / concept\n\
             - \"what places are mentioned\" -> enumerate / place\n\n\
             LOOKUP — it asks for ONE entity, NAMES the specific entit(ies) \
             it is about, or asks to explain / describe / justify a specific \
             thing or event. The decisive test: if the question already \
             names the entities it concerns, it is LOOKUP — it investigates \
             those named things, it does NOT enumerate an unknown set. This \
             holds even when SEVERAL entities are named and even when the \
             phrasing is plural (\"the X and Y partnerships\"). Asking \
             \"which/who\" about a SINGLE entity is LOOKUP, not enumerate.\n\
             - \"who led the negotiation\" (one entity) -> lookup\n\
             - \"what does this say about a specific named deal\" (names its subject) -> lookup\n\
             - \"what do these reveal about the Alpha and Beta partnerships\" (names its subjects, even though several) -> lookup\n\
             - \"describe the agreement\" -> lookup\n\
             - \"why did the project fail\" -> lookup\n\n\
             If enumerate, name the entity_type from: person, institution, \
             initiative, concept, work, place.\n\n\
             Question: {message}\n\n\
             Output only this JSON, nothing after it:\n\
             {{\"mode\": \"enumerate\", \"entity_type\": \"institution\"}}"
        );

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["enumerate", "lookup"]},
                "entity_type": {
                    "type": "string",
                    "enum": ["person", "institution", "initiative", "concept", "work", "place"]
                }
            },
            "required": ["mode"]
        });

        let request = CompletionRequest {
            prompt,
            system_message: None,
            preferred_speed: Speed::Fast,
            max_tokens: Some(40),
            temperature: Some(0.0),
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
                    "atom_enum: Fast-slot classify call failed; skipping enumeration"
                );
                return None;
            }
        };
        let raw = response.text.trim();
        // Glassbox: log the model's actual classify output for EVERY
        // question, before parsing. The enumerate/lookup decision is the
        // load-bearing gate; this line makes "why did (didn't) atom-enum
        // fire here" inspectable in one grep, and surfaces ramble-past-
        // JSON (the Fast slot's known failure: `{...}\n\nWait, let…`).
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_classify",
            query = %truncate_with_ellipsis(message, 80),
            raw = %truncate_with_ellipsis(raw, 240),
            "retrieval_audit: atom_enum classify raw"
        );
        // Tolerate ramble-past-JSON: take the first balanced {...} object
        // rather than requiring the whole reply to be valid JSON.
        let json_str = extract_first_json_object(raw).unwrap_or_else(|| {
            raw.strip_prefix("```json")
                .and_then(|s| s.strip_suffix("```"))
                .unwrap_or(raw)
                .trim()
                .to_string()
        });
        let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::info!(
                    error = %e,
                    raw = %raw,
                    "atom_enum: classify parse failed; skipping enumeration"
                );
                return None;
            }
        };
        // Bias to lookup: anything that is not an explicit `enumerate`
        // verdict (including a missing/garbled mode) is treated as a
        // lookup and short-circuits — no atom noise on focused queries.
        if parsed.get("mode").and_then(|v| v.as_str()) != Some("enumerate") {
            return None;
        }
        let target_type = parsed
            .get("entity_type")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        // ---- Stage 2: enumerate top-salience atoms of that type from
        // the atlas GRAPH. The graph is failure-immune by construction:
        // `AtlasGraph::load_from_disk` inserts every atom, so no-role
        // institutions the embed bag would drop are present here.
        let top_k: usize = std::env::var("SOVEREIGN_ATOM_ENUM_TOPK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&k| k > 0 && k <= 100)
            .unwrap_or(16);

        // Use the enabled-corpora list directly when scoped. The atlas
        // GRAPH can be loaded even when its embedding-bag CONTEXT isn't:
        // a freshly re-enriched atlas has a new atoms.json but a stale
        // embeddings cache, so `load_one` skips the context — yet
        // `AtlasGraph::load_from_disk` still loaded the graph. Keying off
        // `loaded_corpus_ids()` (contexts only) would drop exactly that
        // corpus (observed: enron-sample-multi-wide right after re-enrich
        // → corpora=[] → no enumeration). `provider.graph(id)` below
        // returns None for any id that genuinely has no graph, so an
        // unscoped fallback to loaded contexts is still safe.
        let corpus_ids: Vec<String> = match enabled_corpora {
            Some(enabled) if !enabled.is_empty() => enabled.to_vec(),
            _ => provider.discoverable_corpus_ids(),
        };

        // Prominence per atom: graph degree (in + out edges), tie-broken
        // by alias count then salience. Degree is the real signal — this
        // corpus's salience is a flat 0.70 default and post-reconciliation
        // frequency is uniformly 1, but degree separates the real cast
        // (Lay 923, Dynegy 59 edges) from address-book noise (~0).
        // Graceful: alias/salience only break ties, and cover corpora
        // whose atlas has no edges.json (every degree 0).
        #[derive(Clone)]
        struct Candidate {
            prominence: (usize, usize), // (degree, alias_count)
            salience: f32,
            corpus: String,
            chunk_id: String, // first_appearance.chunk_id (numeric OR "sec_NNNN")
            preview: Option<String>, // passage_preview — FTS key for section-shaped ids
            embed_text: String, // "name. description" — relevance-rank key
        }
        let outranks = |a: &Candidate, b: &Candidate| -> bool {
            a.prominence.cmp(&b.prominence) == std::cmp::Ordering::Greater
                || (a.prominence == b.prominence && a.salience > b.salience)
        };
        // Dedup by canonical name (cross-corpus + intra-corpus variants),
        // keeping the most-prominent record. The cap then bounds the
        // injection regardless of how many atoms of a type the corpus
        // holds (4,525 institutions here, most address-book noise).
        let filter_disabled = std::env::var("SOVEREIGN_ATOM_ENUM_NOFILTER")
            .ok()
            .as_deref()
            == Some("1");
        // Relation-evidence candidates (default on; SOVEREIGN_ATOM_ENUM_RELATIONS=0
        // to ablate). See the relation loop below for the rationale.
        let include_relations = std::env::var("SOVEREIGN_ATOM_ENUM_RELATIONS")
            .ok()
            .as_deref()
            != Some("0");
        let mut best: HashMap<String, Candidate> = HashMap::new();
        for id in &corpus_ids {
            let Some(graph) = provider.graph(id) else {
                continue;
            };
            for view in graph.atoms_of_kind(crate::atlas_context::AtomKindTag::Entity) {
                if view.subtype() != target_type {
                    continue;
                }
                let name = view.name().trim();
                if name.is_empty() {
                    continue;
                }
                // Collective-noun filter (person enumeration). The
                // extractor sometimes types group phrases as person
                // entities ("Enron executives", "Enron management",
                // "Enron analysts"). These paraphrase a "who were the
                // executives" question, so cosine ranks them highly, yet
                // they name no individual and pollute the enumerated set
                // (and crowd out real people like Fastow). A real
                // individual's name never contains a generic group noun;
                // drop person atoms whose name does. Person-only:
                // institutions legitimately contain "Committee"/"Board"
                // (e.g. the Special Committee on Related Party
                // Transactions that actually investigated LJM). Env hatch
                // SOVEREIGN_ATOM_ENUM_NOFILTER=1 disables for ablation.
                if target_type == "person" && !filter_disabled {
                    const GROUP_NOUNS: &[&str] = &[
                        "executives",
                        "executive",
                        "management",
                        "mgmt",
                        "employees",
                        "employee",
                        "team",
                        "staff",
                        "analysts",
                        "analyst",
                        "representatives",
                        "representative",
                        "board",
                        "directors",
                        "director",
                        "members",
                        "member",
                        "officials",
                        "official",
                        "personnel",
                        "leadership",
                        "committee",
                        "everyone",
                        "people",
                        "folks",
                        "others",
                    ];
                    let lname = name.to_lowercase();
                    if lname
                        .split(|c: char| !c.is_alphanumeric())
                        .any(|tok| GROUP_NOUNS.contains(&tok))
                    {
                        continue;
                    }
                }
                let degree = graph.edge_degree(view.id());
                let desc = view.description().trim();
                let embed_text = if desc.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}. {desc}")
                };
                // An Entity's evidence is its single `first_appearance` ref.
                let first = view.evidence().next();
                let cand = Candidate {
                    prominence: (degree, view.alias_count()),
                    salience: view.salience(),
                    corpus: id.clone(),
                    chunk_id: first
                        .as_ref()
                        .map(|ev| ev.chunk_id().to_string())
                        .unwrap_or_default(),
                    preview: first
                        .as_ref()
                        .map(|ev| ev.passage_preview().to_string())
                        .filter(|s| !s.is_empty()),
                    embed_text,
                };
                best.entry(name.to_string())
                    .and_modify(|cur| {
                        if outranks(&cand, cur) {
                            *cur = cand.clone();
                        }
                    })
                    .or_insert(cand);
            }

            // Relation-evidence candidates. For PREDICATE enumerations
            // ("which energy companies are COUNTERPARTIES / competitors")
            // the answer set is defined by a RELATIONSHIP, not an entity
            // type — and an entity's first_appearance chunk proves only
            // that it exists, not that it holds the relationship (so the
            // counterparty turn could name Calpine but never ground it as
            // a counterparty). Relation atoms carry the relationship-
            // bearing evidence chunk directly ("beat out Reliant and TXU",
            // "competing for partnership", "potential acquisition target
            // of"). We add them to the same candidate pool and let the
            // relevance/RRF re-rank surface them when the query is
            // relational — the relation's `label + participants` embeds
            // near the predicate, and the fetched evidence chunk STATES
            // the relationship. On non-relational ("who were the X")
            // queries relations cosine-rank low and the entity atoms win,
            // so this is additive, not a regression to entity enumeration.
            // Keyed by display string so identical relations dedup without
            // colliding with entity names.
            if include_relations {
                for view in graph.atoms_of_kind(crate::atlas_context::AtomKindTag::Relation) {
                    let label = view.label().trim();
                    // First evidence ref grounds the relationship; skip
                    // relations with no label or no evidence (same guard as
                    // the former `label.is_empty() || r.evidence.is_empty()`).
                    let Some(ev) = view.evidence().next() else {
                        continue;
                    };
                    if label.is_empty() {
                        continue;
                    }
                    let parts: Vec<String> = view
                        .participants()
                        .filter_map(|pid| graph.atom(pid))
                        .filter(|a| a.kind() == crate::atlas_context::AtomKindTag::Entity)
                        .filter_map(|a| {
                            let n = a.name().trim();
                            (!n.is_empty()).then(|| n.to_string())
                        })
                        .collect();
                    let display = if parts.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label} ({})", parts.join(", "))
                    };
                    let embed_text = if parts.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label}. {}", parts.join(", "))
                    };
                    let cand = Candidate {
                        // Relations carry no graph degree; cosine rank is
                        // their only RRF signal, which is exactly what a
                        // predicate query rewards.
                        prominence: (0, 0),
                        salience: 0.5,
                        corpus: id.clone(),
                        chunk_id: ev.chunk_id().to_string(),
                        preview: {
                            let p = ev.passage_preview();
                            (!p.is_empty()).then(|| p.to_string())
                        },
                        embed_text,
                    };
                    best.entry(display).or_insert(cand);
                }
            }
        }
        if best.is_empty() {
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_empty",
                target_type = %target_type,
                corpora = ?corpus_ids,
                "atom_enum: no atoms of chosen type in enabled corpora; skipping"
            );
            return None;
        }

        let mut ranked: Vec<(String, Candidate)> = best.into_iter().collect();
        // Base order: prominence (degree) desc, salience, name asc. This
        // is the deterministic fallback when the embedder is unavailable,
        // and the prefilter base when the type-pool exceeds the cost cap.
        ranked.sort_by(|a, b| {
            b.1.prominence
                .cmp(&a.1.prominence)
                .then_with(|| {
                    b.1.salience
                        .partial_cmp(&a.1.salience)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.0.cmp(&b.0))
        });

        // Cost bound for wiki-scale atlases: cap the pool we embed. Enron
        // (284 institutions / 622 persons) is far under the default 800,
        // so this is a no-op here; it stops a 50k-atom type from issuing
        // a 50k-text embed batch. Prefilter is by degree (keeps the real
        // cast); logged so the truncation is never silent.
        let pool_cap: usize = std::env::var("SOVEREIGN_ATOM_ENUM_POOL")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(800);
        let pool_truncated = ranked.len() > pool_cap;
        if pool_truncated {
            ranked.truncate(pool_cap);
        }

        // HYBRID RE-RANK (default = RRF). Neither raw signal generalizes
        // across entity types:
        //   - DEGREE alone ranks the custodian's ego-network (high-degree
        //     address-book hubs — United Way, Moody's) above the sparse
        //     orgs an institution question enumerates (Calpine / El Paso /
        //     Williams, LJM / Marlin).
        //   - RELEVANCE (cosine) alone ranks query-PARAPHRASE atoms
        //     ("Enron executives", "Enron upper mgmt") above the real
        //     people a person question enumerates (Lay / Skilling /
        //     Fastow), because the question text embeds nearest to atoms
        //     that restate it rather than answer it.
        // Reciprocal Rank Fusion (k=60, the codebase's hybrid-search
        // idiom) demands BOTH: a real answer entity ranks well on at
        // least one signal and decently on the other, beating junk that
        // spikes on only one. RRF is also robust to degree's extreme skew
        // (Lay ~923 edges dwarfs every other person), where a linear blend
        // would let one hub crush the normalisation. Embedding is
        // on-the-fly (not the precomputed bag) because a re-enriched atlas
        // has a stale embeddings cache (atoms.json newer than
        // atoms.embeddings.bin). Env hatch SOVEREIGN_ATOM_ENUM_RANK ∈
        // {rrf (default), relevance, degree}; any embedder failure falls
        // back to degree order.
        let rank_mode = std::env::var("SOVEREIGN_ATOM_ENUM_RANK").unwrap_or_else(|_| "rrf".into());
        let mut ranked_by = "degree";
        if rank_mode != "degree" && !ranked.is_empty() {
            // `ranked` is already degree-sorted, so position == degree rank.
            let texts: Vec<String> = ranked.iter().map(|(_, c)| c.embed_text.clone()).collect();
            match (
                self.inference.embed_query(message).await,
                self.inference.embed_batch(&texts).await,
            ) {
                (Ok(q), Ok(embs)) if embs.len() == ranked.len() && !q.is_empty() => {
                    let n = ranked.len();
                    let cosines: Vec<f32> = (0..n)
                        .map(|i| crate::atlas_context::cosine(&q, &embs[i]))
                        .collect();
                    if rank_mode == "relevance" {
                        // Pure cosine (ablation).
                        let mut order: Vec<usize> = (0..n).collect();
                        order.sort_by(|&a, &b| {
                            cosines[b]
                                .partial_cmp(&cosines[a])
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| ranked[a].0.cmp(&ranked[b].0))
                        });
                        ranked = order.into_iter().map(|i| ranked[i].clone()).collect();
                        ranked_by = "relevance";
                    } else {
                        // RRF of degree rank (position) + cosine rank.
                        let mut by_cos: Vec<usize> = (0..n).collect();
                        by_cos.sort_by(|&a, &b| {
                            cosines[b]
                                .partial_cmp(&cosines[a])
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        let mut cos_rank = vec![0usize; n];
                        for (r, &i) in by_cos.iter().enumerate() {
                            cos_rank[i] = r;
                        }
                        const RRF_K: f32 = 60.0;
                        let rrf = |i: usize| -> f32 {
                            1.0 / (RRF_K + i as f32) + 1.0 / (RRF_K + cos_rank[i] as f32)
                        };
                        let mut order: Vec<usize> = (0..n).collect();
                        order.sort_by(|&a, &b| {
                            rrf(b)
                                .partial_cmp(&rrf(a))
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then_with(|| ranked[a].0.cmp(&ranked[b].0))
                        });
                        ranked = order.into_iter().map(|i| ranked[i].clone()).collect();
                        ranked_by = "rrf";
                    }
                }
                _ => { /* embedder unavailable / dim mismatch → keep degree order */ }
            }
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_rank",
            target_type = %target_type,
            ranked_by,
            pool = ranked.len(),
            pool_truncated,
            "atom_enum: candidate ranking ({ranked_by})"
        );
        ranked.truncate(top_k);

        // Inject the enumerated entities DIRECTLY as compact virtual
        // chunks (name + role + description) rather than fanning out a
        // re-search per atom. The atom metadata already carries the
        // answer — "Kenneth Lay — Chairman and Chief Executive" — so one
        // dense item per atom surfaces the fact without the N×limit
        // chunk flood that displaces base hits (measured −0.33 on a
        // person enumeration when re-searching). Scored descending by
        // rank so the most-central members sit highest; SOVEREIGN_ATOM_
        // ENUM_SCORE tunes the band relative to base cosine hits.
        let enum_score: f32 = std::env::var("SOVEREIGN_ATOM_ENUM_SCORE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|&s| s > 0.0)
            .unwrap_or(0.04);

        // Atlas DIRECTS retrieval. For each enumerated entity, fetch its
        // REAL evidence chunk (`first_appearance.chunk_id`) from the
        // corpus index rather than synthesising a name+role virtual
        // chunk. This is the load-bearing fix and the architectural
        // contract: the enrichment graph says WHICH chunks the question
        // needs; the normal pipeline then ranks them. Real chunks carry
        // real content + a real `chunk_id`, so — unlike virtual chunks —
        // they survive corpus-isolation, dedup, and the synthesis
        // snapshot, and they earn their slot via `reweight_by_query_
        // relevance` on actual text instead of a hand-set score (which
        // reweight would clobber anyway). Resolution is shape-aware:
        // numeric chunk_id → direct LanceDB fetch; section-shaped id
        // ("sec_0001", the modern pipelines) → FTS the passage_preview
        // for the evidence chunk (per atom). An unresolvable atom is a
        // no-op.
        let mut chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut fetched_names: Vec<&str> = Vec::new();
        for (i, (name, c)) in ranked.iter().enumerate() {
            // Shape-aware resolution to a REAL chunk. Numeric chunk_id
            // (legacy corpus-mode atoms) → direct LanceDB fetch.
            // Section-shaped id ("sec_0001", the modern pipelines) has no
            // direct row → FTS the corpus for its passage_preview and
            // take the top hit (the atom's evidence chunk). Either way
            // the result is a real chunk (real id + content) that earns
            // its rank via reweight and survives the pipeline.
            let mut fetched = match c.chunk_id.trim().parse::<u64>() {
                Ok(cid) => self.fetch_chunk_by_id(&c.corpus, cid).await,
                Err(_) => None,
            };
            if fetched.is_none() {
                if let Some(pv) = c
                    .preview
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    fetched = self
                        .search_corpus_indexes_with_overrides(
                            &[],
                            pv,
                            1,
                            "AtomEnum",
                            None,
                            enabled_corpora,
                            corpus_ceiling,
                        )
                        .await
                        .into_iter()
                        .next();
                }
            }
            let Some(mut chunk) = fetched else {
                continue;
            };
            // Seed score; reweight overwrites it from real content
            // overlap. The taper only orders ties before reweight runs.
            chunk.score = enum_score * 0.96_f32.powi(i as i32);
            chunk
                .metadata
                .insert("source".to_string(), "atom-enum".to_string());
            chunk
                .metadata
                .insert("atom_entity".to_string(), name.clone());
            chunk
                .metadata
                .insert("entity_type".to_string(), target_type.clone());
            chunks.push(chunk);
            fetched_names.push(name.as_str());
        }
        if chunks.is_empty() {
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_nofetch",
                target_type = %target_type,
                candidates = ranked.len(),
                sample = ?ranked.iter().take(3).map(|(n, c)| format!("{n}|cid={}|pv={}", c.chunk_id, c.preview.is_some())).collect::<Vec<_>>(),
                "atom_enum: candidates found but no evidence chunks fetched"
            );
            return None;
        }

        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum",
            query = %truncate_with_ellipsis(message, 120),
            entity_type = %target_type,
            count = chunks.len(),
            names = ?fetched_names,
            "retrieval_audit: atom_enum directed-fetch"
        );
        Some(chunks)
    }

    /// Overview/summary grounding: inject the scoped corpus's atlas Claim
    /// atoms as compact virtual chunks. An overview question has no entity
    /// anchor, so normal retrieval returns a diffuse pool the grounding gate
    /// can't tie to "the most important thing" — and the answer abstains or
    /// confabulates a theme. The atlas Claims ARE the corpus's key points
    /// (e.g. maple-house's 67 charter rules), pre-extracted and grounded by
    /// construction. Tagged `source=atom-enum` so the `cap_and_reserve`
    /// atom-enum reserve carries them through truncation (their reweight score
    /// is irrelevant — an overview query has no lexical anchor to reweight on).
    /// Claims that carry a verbatim `quotable_excerpt` rank first (the answer
    /// can quote the corpus's own words); `confidence` breaks ties. Returns
    /// `None` when the scoped corpus holds no Claim atoms (entity-only atlas,
    /// or none) so the caller falls through to the normal pool. Gated by
    /// `SOVEREIGN_ATOM_ENUM_OVERVIEW`.
    async fn enumerate_overview_claim_chunks(
        &self,
        message: &str,
        enabled_corpora: Option<&[String]>,
        corpus_ceiling: Option<&[String]>,
    ) -> Option<Vec<corpus_engine::ScoredChunk>> {
        let provider = self.atlas_context_provider.as_ref()?;
        let top_k: usize = std::env::var("SOVEREIGN_ATOM_ENUM_TOPK")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&k| k > 0 && k <= 100)
            .unwrap_or(16);
        let enum_score: f32 = std::env::var("SOVEREIGN_ATOM_ENUM_SCORE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|&s| s > 0.0)
            .unwrap_or(0.04);
        let corpus_ids: Vec<String> = match enabled_corpora {
            Some(enabled) if !enabled.is_empty() => enabled.to_vec(),
            _ => provider.discoverable_corpus_ids(),
        };
        struct ClaimCand {
            content: String,
            excerpt: Option<String>,
            corpus: String,
            has_excerpt: bool,
            confidence: f32,
            // Evidence pointer (the claim's first ChunkRef) — lets the injector
            // resolve the claim to its REAL source chunk (MAP) instead of
            // injecting the atom's paraphrased `content` (DATA). `None` for
            // derived claims that carry no evidence.
            evidence_chunk_id: Option<String>,
            evidence_preview: Option<String>,
            has_evidence: bool,
        }
        let mut cands: Vec<ClaimCand> = Vec::new();
        for id in &corpus_ids {
            let Some(graph) = provider.graph(id) else {
                continue;
            };
            for view in graph.atoms_of_kind(crate::atlas_context::AtomKindTag::Claim) {
                let content = view.content().trim();
                if content.is_empty() {
                    continue;
                }
                let excerpt = {
                    let e = view.excerpt().trim();
                    (!e.is_empty()).then(|| e.to_string())
                };
                let ev = view.evidence().next();
                cands.push(ClaimCand {
                    content: content.to_string(),
                    has_excerpt: excerpt.is_some(),
                    excerpt,
                    corpus: id.clone(),
                    confidence: view.confidence(),
                    evidence_chunk_id: ev.as_ref().map(|e| e.chunk_id().to_string()),
                    evidence_preview: ev.as_ref().and_then(|e| {
                        let p = e.passage_preview();
                        (!p.is_empty()).then(|| p.to_string())
                    }),
                    has_evidence: ev.is_some(),
                });
            }
        }
        if cands.is_empty() {
            return None;
        }
        // Rank to maximise REAL-chunk grounding in the kept top-k: claims that
        // carry a resolvable evidence chunk (→ MAP to a real source chunk) come
        // first, then claims with a verbatim quote, then higher extraction
        // confidence.
        cands.sort_by(|a, b| {
            b.has_evidence
                .cmp(&a.has_evidence)
                .then_with(|| b.has_excerpt.cmp(&a.has_excerpt))
                .then_with(|| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        cands.truncate(top_k);
        let mut chunks: Vec<corpus_engine::ScoredChunk> = Vec::with_capacity(cands.len());
        let mut seen_chunk_ids: std::collections::HashSet<(String, u64)> =
            std::collections::HashSet::new();
        let mut mapped = 0usize;
        for (i, c) in cands.iter().enumerate() {
            // Gentle taper preserves the evidence/quote/confidence order before
            // reweight; the cap reserve (not the score) carries these through
            // truncation, and reweight overwrites it from real content on the
            // MAP chunks.
            let seed_score = enum_score * 0.99_f32.powi(i as i32);

            // MAP-first: resolve the claim's evidence to its REAL source chunk —
            // the SAME shape-aware resolution the entity-enumeration path uses
            // (numeric chunk_id → direct LanceDB fetch; section-shaped id → FTS
            // the passage_preview for the evidence chunk). The answer then
            // grounds on the article's actual text with a real chunk_id, not the
            // atom's propositional paraphrase, and the real chunk survives
            // dedup + the synthesis snapshot. DATA injection is the fallback ONLY
            // when the claim has no resolvable evidence (derived claims, stale
            // ids) — so an overview corpus still gets its key points either way.
            let resolved = match c
                .evidence_chunk_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(cid) => {
                    let mut got = match cid.parse::<u64>() {
                        Ok(n) => self.fetch_chunk_by_id(&c.corpus, n).await,
                        Err(_) => None,
                    };
                    if got.is_none() {
                        if let Some(pv) = c
                            .evidence_preview
                            .as_deref()
                            .map(str::trim)
                            .filter(|p| !p.is_empty())
                        {
                            got = self
                                .search_corpus_indexes_with_overrides(
                                    &[],
                                    pv,
                                    1,
                                    "AtomEnumOverview",
                                    None,
                                    enabled_corpora,
                                    corpus_ceiling,
                                )
                                .await
                                .into_iter()
                                .next();
                        }
                    }
                    got
                }
                None => None,
            };

            if let Some(mut chunk) = resolved {
                // Claims often cluster on one section — skip a duplicate evidence
                // chunk (keep the higher-ranked claim's). dedupe_merged would
                // catch content dupes downstream too, but this avoids a redundant
                // fetch occupying an atom-enum reserve slot.
                if let Some(cid) = chunk.chunk_id {
                    if !seen_chunk_ids.insert((chunk.corpus_id.clone(), cid)) {
                        continue;
                    }
                }
                chunk.score = seed_score;
                chunk
                    .metadata
                    .insert("source".to_string(), "atom-enum".to_string());
                chunk
                    .metadata
                    .insert("atom_type".to_string(), "claim".to_string());
                chunks.push(chunk);
                mapped += 1;
            } else {
                // DATA fallback: inject the claim text (+ verbatim quote when
                // present). Tagged `atom_claim_unmapped` so the glassbox shows
                // which overview chunks are synthetic vs resolved-to-source.
                let content = match &c.excerpt {
                    Some(q) => format!("{}\n\nSource quote: \"{}\"", c.content, q),
                    None => c.content.clone(),
                };
                let mut metadata = std::collections::HashMap::new();
                metadata.insert("source".to_string(), "atom-enum".to_string());
                metadata.insert("atom_type".to_string(), "claim".to_string());
                metadata.insert("atom_claim_unmapped".to_string(), "1".to_string());
                chunks.push(corpus_engine::ScoredChunk {
                    content,
                    title: Some(format!("{} — key point", c.corpus)),
                    url: None,
                    corpus_id: c.corpus.clone(),
                    score: seed_score,
                    metadata,
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                });
            }
        }
        if chunks.is_empty() {
            return None;
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_overview",
            query = %truncate_with_ellipsis(message, 120),
            count = chunks.len(),
            mapped_to_real_chunks = mapped,
            data_fallback = chunks.len() - mapped,
            corpora = ?corpus_ids,
            "retrieval_audit: atom_enum overview-claim injection (MAP-first; DATA fallback for unresolvable claims)"
        );
        Some(chunks)
    }

    /// Question-shape heuristic for the overview/summary claim path. No LLM
    /// call — a corpus-level "what matters here" ask is recognisable from
    /// phrasing alone, and keeping it cheap means it can run on every turn the
    /// flag is set. Deliberately broad: a false positive just augments the
    /// pool with the corpus's key points (bounded, atom-enum-tagged), which is
    /// harmless; a false negative falls through to normal retrieval.
    fn looks_like_overview(message: &str) -> bool {
        let m = message.to_lowercase();
        const MARKERS: &[&str] = &[
            "most important",
            "summar", // summary / summarize / summarise
            "overview",
            "main point",
            "main idea",
            "main theme",
            "main takeaway",
            "key point",
            "key idea",
            "key theme",
            "key takeaway",
            "the gist",
            "tell me about",
            "what is this about",
            "what's this about",
            "what is it about",
            "what are these about",
            "high level",
            "high-level",
        ];
        MARKERS.iter().any(|k| m.contains(k))
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
        chunks: &mut [corpus_engine::ScoredChunk],
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
            conv_indices
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &i| {
                    let s = chunks[i].score;
                    (lo.min(s), hi.max(s))
                });
        let cosine_range = (cosine_max - cosine_min).max(1e-6);

        let (mass_min, mass_max) = idx_to_mass
            .values()
            .copied()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            });
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
                chunks[i]
                    .metadata
                    .insert("ppr_mass_norm".to_string(), format!("{:.3}", entity_norm));
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
        let payload =
            crate::conv_briefing::build_conv_tiered_briefings(reader, chunks, cats_opt).await;
        if !payload.rendered.is_empty() {
            tracing::debug!(
                mode = payload.mode.label(),
                convs = payload.conv_count,
                "conv_briefing: surfaced tiered context"
            );
        }
        let vault_block =
            crate::conv_briefing::build_vault_synthesis_briefings(reader, chunks, cats_opt).await;
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

    /// The embedding dim of a corpus's RAPTOR summary index IF a FRESH one
    /// exists (built at/after the newest `conv_raptor_nodes.created_at`), else
    /// `None` → the caller falls back to the scan. The freshness probe is a
    /// tiny sidecar read plus one `MAX(created_at)` aggregate
    /// (`corpus_raptor_version`) — far cheaper than the full-table BLOB decode
    /// the brute-force scan performs. Staleness never triggers an inline
    /// rebuild (that latency spike is exactly what late injection avoids); the
    /// operator rebuilds via `sovereign enrich raptor-index`.
    async fn raptor_index_dim_if_fresh(&self, corpus_id: &str) -> Option<usize> {
        let engine = self.corpus_engine.as_ref()?;
        let meta = engine.raptor_index_meta(corpus_id)?;
        let reader = self.conv_tiered_reader.as_ref()?;
        let live = reader.corpus_raptor_version(corpus_id).await.ok()?;
        if meta.source_version >= live {
            Some(meta.dim)
        } else {
            tracing::info!(
                corpus = %corpus_id,
                built_version = meta.source_version,
                live_version = live,
                "raptor-grounding: summary index stale — scanning (run `sovereign enrich raptor-index`)"
            );
            None
        }
    }

    /// ANN-index candidates for one corpus, or `None` to signal "scan instead"
    /// (index absent / stale / dim-mismatch / empty / `min_level` under-fill).
    /// Over-fetches `fetch_m` and filters `level >= min_level` in Rust — the
    /// `only_if` + `nearest_to` push-down is unverified on lancedb 0.27, and M
    /// is tiny. The scan fallback filters `min_level` at the SQL boundary, so
    /// it never under-fills; when the over-fetched ANN set does, we defer to it.
    async fn raptor_index_candidates(
        &self,
        corpus_id: &str,
        embedding: &[f32],
        fetch_m: usize,
        top_m: usize,
        min_level: i64,
    ) -> Option<Vec<RaptorCand>> {
        let engine = self.corpus_engine.as_ref()?;
        let dim = self.raptor_index_dim_if_fresh(corpus_id).await?;
        if dim != embedding.len() {
            tracing::warn!(
                corpus = %corpus_id,
                table_dim = dim,
                query_dim = embedding.len(),
                "raptor-grounding: index dim mismatch — scanning"
            );
            return None;
        }
        let hits = engine
            .search_raptor_summaries(corpus_id, embedding, fetch_m)
            .await
            .ok()?;
        let cands: Vec<RaptorCand> = hits
            .into_iter()
            .filter(|h| h.level >= min_level)
            .map(|h| RaptorCand {
                score: h.score,
                conv_uuid: h.conv_uuid,
                corpus_id: corpus_id.to_string(),
                level: h.level,
                summary: h.summary,
            })
            .collect();
        if cands.is_empty() || (min_level > 0 && cands.len() < top_m) {
            return None;
        }
        Some(cands)
    }

    /// RAPTOR collapsed-tree grounding (`SOVEREIGN_RAPTOR_GROUNDING`, default ON
    /// — set `=0` to disable). Late-injected by default (`raptor_late_inject_
    /// enabled`) so it's QA-neutral on the SEP bench; on by default for the
    /// whole-work summarization capability it adds. The relevance pass prefers
    /// a per-corpus LanceDB ANN index (`raptor_summaries.lance`, built by
    /// `enrich raptor` / `enrich raptor-index`) via `raptor_index_candidates`,
    /// and falls back to a brute-force cosine scan over `conv_raptor_nodes`
    /// when no FRESH index exists — so a corpus without an index still works,
    /// just at scan throughput (the path the index removes at wiki scale).
    /// Cosines the query embedding against the queried corpora's RAPTOR
    /// summary-node embeddings (`conv_raptor_nodes`), takes the global
    /// top-M, and injects each as a virtual `ScoredChunk` — so a query can
    /// match a whole-document / section SUMMARY even when no leaf chunk
    /// surfaced. The summary's `title` is the source-doc slug (so it counts
    /// toward source coverage) and `source_doc_id` back-points to the
    /// origin. Mirrors `apply_atlas_grounding`'s bag-of-atoms shape. Tunable:
    /// `SOVEREIGN_RAPTOR_TOP_M` (default 8), `SOVEREIGN_RAPTOR_MIN_LEVEL`
    /// (default 0 = all nodes incl. leaves; 1 = section/doc summaries only).
    pub(crate) async fn apply_raptor_grounding(
        &self,
        embedding: &[f32],
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        label: &str,
        enabled_corpora: Option<&[String]>,
    ) {
        let enabled = std::env::var("SOVEREIGN_RAPTOR_GROUNDING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        if !enabled {
            return;
        }
        let Some(reader) = self.conv_tiered_reader.as_ref() else {
            return;
        };
        if embedding.is_empty() {
            return;
        }
        let top_m: usize = std::env::var("SOVEREIGN_RAPTOR_TOP_M")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);
        if top_m == 0 {
            return;
        }
        let min_level: i64 = std::env::var("SOVEREIGN_RAPTOR_MIN_LEVEL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        // Which corpora to ground: the conversation allow-list when set
        // (the bench's --isolate path), else the distinct corpora that
        // already produced hits this turn.
        let corpus_ids: Vec<String> = match enabled_corpora {
            Some(allowed) if !allowed.is_empty() => allowed.to_vec(),
            _ => {
                let mut s: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
                for c in chunks.iter() {
                    s.insert(c.corpus_id.clone());
                }
                s.into_iter().collect()
            }
        };
        if corpus_ids.is_empty() {
            return;
        }
        // Optional dedupe-by-article (SOVEREIGN_RAPTOR_DEDUPE=1, default off);
        // read up-front so we can size the over-fetch.
        let dedupe_by_article = std::env::var("SOVEREIGN_RAPTOR_DEDUPE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        // Over-fetch from the ANN index so the post-merge `min_level` filter +
        // dedupe still leave M *distinct* works after truncation. M is tiny.
        let fetch_m = top_m.saturating_mul(8).max(8);

        // Glassbox: count which path served each corpus so an operator can
        // see whether grounding took the fast ANN index or the brute-force
        // scan fallback (and catch a corpus silently degrading to the scan).
        let mut via_index = 0usize;
        let mut via_scan = 0usize;
        let mut scored: Vec<RaptorCand> = Vec::new();
        for corpus_id in &corpus_ids {
            // Prefer the ANN index (`raptor_summaries.lance`); fall back to the
            // brute-force scan when it's absent, stale, dim-mismatched, or a
            // `min_level` filter under-fills the over-fetched set. Both paths
            // funnel into the same `RaptorCand` → byte-identical injected
            // chunks via `raptor_scored_chunk`.
            if let Some(cands) = self
                .raptor_index_candidates(corpus_id, embedding, fetch_m, top_m, min_level)
                .await
            {
                via_index += 1;
                scored.extend(cands);
                continue;
            }
            let nodes = match reader.list_corpus_raptor_nodes(corpus_id, min_level).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(label, corpus = %corpus_id, error = %e,
                        "raptor-grounding: list_corpus_raptor_nodes failed");
                    continue;
                }
            };
            via_scan += 1;
            for node in nodes {
                if node.summary_embedding.len() != embedding.len() {
                    continue;
                }
                let s = crate::atlas_context::cosine(embedding, &node.summary_embedding);
                scored.push(RaptorCand {
                    score: s,
                    conv_uuid: node.conv_uuid,
                    corpus_id: node.corpus_id,
                    level: node.level,
                    summary: node.summary,
                });
            }
        }
        if scored.is_empty() {
            return;
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // Dedupe-by-article semantics (SOVEREIGN_RAPTOR_DEDUPE):
        // A long entry has summaries at every tree level — level-0 leaf
        // clusters through the level-N root — all keyed to the same slug, and
        // all score high on a query about that entry. On a source-coverage QA
        // query that lets one article flood the top-M (observed: goedel ×6,
        // kant-hume-causality ×7 in one 8-slot injection), so deduping to M
        // *distinct* works improves QA diversity. BUT for whole-work SUMMARY
        // intent those multi-level nodes are COMPLEMENTARY (each summarizes a
        // different section), so deduping costs summary depth — hence opt-in,
        // not default. Additive truncation (the merge sites) already removes
        // the displacement harm without this tradeoff; intent-conditional
        // dedupe is the proper long-term home once a summary-intent signal
        // exists. Kept as a flag so the two levers stay independently testable.
        if dedupe_by_article {
            let mut seen = std::collections::HashSet::new();
            scored.retain(|c| seen.insert(c.conv_uuid.clone()));
        }
        scored.truncate(top_m);
        let added = scored.len();
        for c in scored {
            chunks.push(raptor_scored_chunk(
                c.conv_uuid,
                c.corpus_id,
                c.level,
                c.summary,
                c.score,
            ));
        }
        tracing::info!(label, added, top_m, min_level, via_index, via_scan,
            "raptor-grounding: collapsed-tree summaries injected");
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
        corpus_ceiling: Option<&[String]>,
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
                    corpus_engine::CorpusKind::Knowledge | corpus_engine::CorpusKind::Catalog
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
        let eligible: Vec<_> = indexes
            .into_iter()
            .filter(|info| {
                // Readiness: an index that never finished building (ingest
                // stalled / sync paused) has no searchable content — skip it on
                // EVERY path so the model can't fabricate over the void. The
                // readiness disclosure step surfaces a rebuild prompt when the
                // SCOPED corpus is the cause.
                if !info.indexes_built {
                    tracing::debug!(
                        corpus = %info.corpus_id,
                        "{label}: skipping corpus — index not built (rebuild/resume needed)"
                    );
                    return false;
                }
                // The vector + dimension checks apply only to the vector path
                // (query_dims != 0); the FTS-only path keeps every built index
                // so it can still serve its BM25 results.
                if query_dims != 0 {
                    if !info.vector_index_built {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            "{label}: skipping corpus — vector index missing (rebuild needed)"
                        );
                        return false;
                    }
                    if info.embedding_dimensions != query_dims {
                        tracing::debug!(
                            corpus = %info.corpus_id,
                            stored_dims = info.embedding_dimensions,
                            query_dims,
                            embedding_model = %info.embedding_model,
                            "{label}: skipping corpus — embedding-dimension mismatch"
                        );
                        return false;
                    }
                }
                true
            })
            .collect();
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

        // Filter 5 — per-principal retrieval ceiling (multi-tenant hub).
        //
        // The AIRTIGHT upper bound, and the ONLY corpus filter that is a
        // security boundary. On a multi-tenant hub the server injects a
        // `PrincipalResolver`; `build_context` then stamps this conversation's
        // ceiling = `{Org corpora} ∪ {Private corpora the principal owns}`.
        // We re-apply the SAME parent-aware allow-list filter — but with the
        // ceiling, INDEPENDENT of the user-controlled `enabled_corpora`
        // (Filter 4). That independence is the whole point: Filter 4 is a
        // no-op on `None` (the default), so a client that sends no selection —
        // or forges one naming another tenant's Private corpus — is bounded
        // ONLY here. Filter 5 drops every index whose corpus (or parent
        // corpus) is outside the ceiling, so cross-tenant content can never
        // enter the merged pool regardless of what Filter 4 let through.
        //
        // `None` (single-user / desktop — no principal injected) ⇒ no-op, so
        // retrieval is bit-identical to pre-multi-tenant behaviour. A
        // `Some(empty)` ceiling (a principal with zero visible corpora)
        // correctly yields zero eligible indexes — fail-closed, not fail-open.
        // See `ConversationContext::corpus_ceiling`.
        let eligible_pre_ceiling = eligible.len();
        let eligible = apply_corpus_allow_list(eligible, corpus_ceiling);
        if eligible.len() < eligible_pre_ceiling {
            tracing::info!(
                target: "retrieval.isolation",
                label = %label,
                ceiling = ?corpus_ceiling,
                eligible_after = eligible.len(),
                ceiling_dropped = eligible_pre_ceiling - eligible.len(),
                "retrieval.isolation: principal-ceiling excluded cross-tenant indexes"
            );
        }

        // Per-corpus fan-out. Concurrency is env-gated
        // (`SOVEREIGN_KQ_FANOUT_CONCURRENCY`) and defaults to 4 (2026-06-26: was
        // the historical serial 1). This is BEHAVIOUR-IDENTICAL: every corpus
        // still pours into one merged pool that is sorted/capped downstream, so
        // concurrency changes only WALL-TIME, never results — which is what makes
        // raising it the SAFE way to bound an UNSCOPED turn's latency (no corpus
        // is dropped, so the answer's corpus is never lost). On a many-corpus
        // unscoped turn the serial fan-out was the dominant retrieval latency
        // (~2s/corpus × N ≈ 60s at N=29); 4-way concurrency collapses that ~4×
        // toward the slowest single corpus. Bounded at a moderate default (not
        // unbounded) so a wide fan-out can't thundering-herd the big indexes
        // (sep/wikipedia) on open + search. Per-corpus + total timing is emitted
        // on `retrieval_audit` so a run can prove where the latency went.
        use futures::StreamExt as _;
        let concurrency = std::env::var("SOVEREIGN_KQ_FANOUT_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(4);
        // Owned, shareable handles so each task captures only owned values (no
        // fn-scope borrow across the .await — that trips the higher-ranked-
        // lifetime / Send bound). We build a Vec of owned futures in a sync loop
        // (each one owns its clones) then drive them with bounded concurrency —
        // the closure-returning-async-block-over-`&info` form does NOT satisfy
        // the HRTB the stream wants, but a Vec of concrete futures does.
        let engine_arc = std::sync::Arc::clone(engine);
        let embedding_arc: std::sync::Arc<[f32]> = std::sync::Arc::from(embedding);
        let query_arc: std::sync::Arc<str> = std::sync::Arc::from(query_text);
        let label_arc: std::sync::Arc<str> = std::sync::Arc::from(label);
        let rerank_fn = self.rerank_fn.clone();
        let rerank_base = self.rerank_config.clone();
        let rerank_enabled = self.rerank_config.enabled && self.rerank_fn.is_some();
        let fanout_t0 = std::time::Instant::now();
        let mut tasks = Vec::with_capacity(eligible.len());
        for info in &eligible {
            let engine = std::sync::Arc::clone(&engine_arc);
            let embedding = std::sync::Arc::clone(&embedding_arc);
            let query_text = std::sync::Arc::clone(&query_arc);
            let label = std::sync::Arc::clone(&label_arc);
            let rerank_fn = rerank_fn.clone();
            let effective_limit = per_corpus_limits
                .and_then(|m| m.get(&info.corpus_id).copied())
                .unwrap_or(limit);
            // Per-corpus effective rerank config: opts this corpus into
            // source-dedup when its recipe declared it (recipe-driven SEP
            // promotion), otherwise the runtime base config unchanged.
            let corpus_rerank = rerank_config_for_corpus(&rerank_base, info);
            let corpus_id = info.corpus_id.clone();
            let path = info.path.clone();
            let chunk_count = info.chunk_count;
            let dims = info.embedding_dimensions;
            let embed_model = info.embedding_model.clone();
            tasks.push(async move {
                let corpus_t0 = std::time::Instant::now();
                tracing::info!(
                    corpus = %corpus_id,
                    path = %path.display(),
                    chunks = chunk_count,
                    dims = dims,
                    embedding_model = %embed_model,
                    "{label}: opening index"
                );
                let idx = match engine.open_index(&path).await {
                    Ok(i) => i,
                    Err(e) => {
                        tracing::warn!(corpus = %corpus_id, error = %e, "{label}: open_index failed");
                        return Vec::new();
                    }
                };
                if effective_limit != limit {
                    tracing::info!(
                        corpus = %corpus_id,
                        base_limit = limit,
                        effective_limit,
                        "{label}: per-corpus K override applied"
                    );
                }
                match idx
                    .search_with_rerank(
                        &embedding,
                        &query_text,
                        effective_limit,
                        rerank_fn.as_ref(),
                        &corpus_rerank,
                        None,
                    )
                    .await
                {
                    Ok(scored) => {
                        let elapsed_ms = corpus_t0.elapsed().as_millis() as u64;
                        tracing::info!(
                            corpus = %corpus_id,
                            results = scored.len(),
                            elapsed_ms,
                            rerank_enabled,
                            "{label}: search complete"
                        );
                        // Naturalistic audit: top-3 per corpus so post-mortem
                        // can answer "did the right article even reach the merge
                        // pool from this corpus?" before any cap or expansion.
                        let top3: Vec<(String, f32)> = scored
                            .iter()
                            .take(3)
                            .map(|c| (c.title.clone().unwrap_or_default(), c.score))
                            .collect();
                        tracing::info!(
                            target: "retrieval_audit",
                            event = "corpus_results",
                            label = %label,
                            corpus = %corpus_id,
                            count = scored.len(),
                            effective_limit,
                            elapsed_ms,
                            top3 = ?top3,
                            "retrieval_audit: corpus_results"
                        );
                        scored
                    }
                    Err(e) => {
                        tracing::warn!(corpus = %corpus_id, error = %e, "{label}: search failed");
                        Vec::new()
                    }
                }
            });
        }
        let per_corpus: Vec<Vec<corpus_engine::ScoredChunk>> =
            futures::stream::iter(tasks).buffer_unordered(concurrency).collect().await;
        for scored in per_corpus {
            chunks.extend(scored);
        }
        tracing::info!(
            target: "retrieval_audit",
            event = "fanout_complete",
            label = label,
            corpora = eligible.len(),
            concurrency,
            fanout_ms = fanout_t0.elapsed().as_millis() as u64,
            merged = chunks.len(),
            "retrieval_audit: fanout_complete"
        );

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
        corpus_ceiling: Option<&[String]>,
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
        // Filter 5 — per-principal retrieval ceiling (multi-tenant hub).
        // The independent, airtight bound — twin of the one in
        // `search_corpus_indexes_with_overrides`. `None` (single-user) ⇒
        // no-op. A forged/over-broad `name_match` or `enabled_corpora` (and
        // even a deliberate exemption like `bridge_boost`'s) cannot widen
        // retrieval past the principal's `{Org} ∪ {owned Private}` corpora.
        let eligible = apply_corpus_allow_list(eligible, corpus_ceiling);

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
            let corpus_rerank = rerank_config_for_corpus(&self.rerank_config, info);
            match idx
                .search_with_rerank(
                    embedding,
                    query_text,
                    limit,
                    self.rerank_fn.as_ref(),
                    &corpus_rerank,
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
        corpus_ceiling: Option<&[String]>,
    ) -> Vec<MetaAtlasHitRecord> {
        // Clone the `Arc` out and drop the guard before the awaits below
        // (`index` is consulted across them; a std `RwLock` guard is not
        // `Send`). `None` until the desktop's deferred warm attaches the
        // index — boost simply short-circuits until then.
        let index = self.meta_atlas.read().ok().and_then(|g| g.clone());
        let Some(index) = index else {
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

            for axis in corpus_engine::stream_axes::Articulation::ALL.iter() {
                let anchor = match corpus_engine::meta_atlas::MetaAtlasIndex::top_anchor_for_axis(
                    &atom,
                    *axis,
                    MIN_AXIS_WEIGHT,
                ) {
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
                        corpus_ceiling,
                    )
                    .await;
                let stability_tag = anchor.stability.map(|s| s.as_str().to_string());
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

    /// Cross-corpus bridge boost (gated `SOVEREIGN_META_BRIDGE`, default
    /// OFF — opt-in). For each question entity that matches a bridge
    /// topic, fetch the LINKED corpus's framing through the typed edge
    /// and inject it, so a query that only hit one corpus still receives
    /// the other's treatment (the "stereo" view). Injected chunks are
    /// stamped `bridge_relation` + `bridge_confidence` for trace/explain.
    /// Returns the number of chunks added. `None`/empty index = no-op.
    pub(crate) async fn bridge_boost(
        &self,
        chunks: &mut Vec<corpus_engine::ScoredChunk>,
        entities: &[String],
        // The live retrieval query — text + its already-computed embedding
        // — used to make the cross-corpus fetch query-aware (steer the pull
        // toward what the user actually asked, not just the bridged topic).
        query: &str,
        query_embedding: &[f32],
        // Intentionally unused: the bridge reaches the linked corpus even
        // when the turn is scoped (see the fetch below).
        _enabled_corpora: Option<&[String]>,
        // NOT exempt, unlike `_enabled_corpora` above. The per-principal
        // ceiling is a security boundary, not a display scope: a bridge edge
        // may steer retrieval to a LINKED corpus the user didn't select, but
        // it must never cross into a corpus the principal doesn't own. So the
        // ceiling is forwarded to Filter 5 in `search_corpora_filtered`.
        corpus_ceiling: Option<&[String]>,
    ) -> usize {
        // Topic-vs-query mix for the cross-corpus fetch embedding. 0.5 =
        // equal weight: the topic anchor keeps the pull inside the linked
        // subject's region of the other corpus, the live query steers to
        // the chunk that answers *this* question. The single tuning point.
        const ANCHOR_WEIGHT: f32 = 0.5;
        let on = std::env::var("SOVEREIGN_META_BRIDGE")
            .ok()
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes"))
            .unwrap_or(false);
        if !on {
            return 0;
        }
        let Some(index) = self.bridge.as_ref() else {
            return 0;
        };
        if index.is_empty() || entities.is_empty() {
            return 0;
        }

        let top_score = chunks
            .iter()
            .map(|c| c.score)
            .fold(f32::MIN, f32::max)
            .max(1.0);
        let mut rank: usize = 0;
        let mut added: usize = 0;
        // Fetch each linked topic at most once across all entities.
        let mut fetched: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        let mut matched_entities = 0usize;
        for entity in entities {
            let elist = index.lookup(entity);
            if !elist.is_empty() {
                matched_entities += 1;
            }
            for edge in elist {
                let other = edge.other_side(entity);
                if !fetched.insert(format!("{}::{}", other.corpus_id, other.title)) {
                    continue;
                }
                let anchor = self
                    .inference
                    .embed_query(&other.title)
                    .await
                    .unwrap_or_default();
                if anchor.is_empty() {
                    continue;
                }
                // Query-aware fetch: blend the topic anchor with the live
                // query embedding so the pull lands on the chunk that
                // answers THIS question, while staying inside the linked
                // topic's region. Topic-only fallback when the query
                // embedding is absent (see `blend_query_aware`).
                let emb = blend_query_aware(&anchor, query_embedding, ANCHOR_WEIGHT);
                // Exempt from `enabled_corpora`: reaching the LINKED corpus
                // is the bridge's entire purpose, so it must fetch
                // `other.corpus_id` even when the turn's retrieval is scoped
                // away from it (e.g. a SEP-sealed turn pulling the linked
                // Wikipedia article through a typed edge). `name_match`
                // still pins the fetch to exactly that corpus.
                // The rerank text goes query-aware too: topic title + the
                // user's question, so lexical/rerank signals favour query
                // terms within the linked topic. The corpus is still pinned
                // by `other.corpus_id` (the name_match arg), not this text.
                let fetch_text = if query.is_empty() {
                    other.title.clone()
                } else {
                    format!("{} {}", other.title, query)
                };
                let hits = self
                    .search_corpora_filtered(
                        &emb,
                        &fetch_text,
                        CANONICAL_PRIMARY_LIMIT,
                        None,
                        Some(&other.corpus_id),
                        "BridgeBoost",
                        None,
                        corpus_ceiling,
                    )
                    .await;
                let relation = edge.relation.as_str();
                let confidence = format!("{:.2}", edge.confidence);
                for mut hit in hits {
                    if hit.corpus_id != other.corpus_id {
                        continue;
                    }
                    rank += 1;
                    let lifted = top_score + 1e-4 * (rank as f32);
                    // Already present: lift score + tag in place.
                    if let Some(existing) = chunks.iter_mut().find(|c| {
                        c.corpus_id == hit.corpus_id
                            && c.chunk_id.is_some()
                            && c.chunk_id == hit.chunk_id
                    }) {
                        existing.score = lifted;
                        existing
                            .metadata
                            .insert("bridge_relation".to_string(), relation.to_string());
                        existing
                            .metadata
                            .insert("bridge_confidence".to_string(), confidence.clone());
                        continue;
                    }
                    hit.score = lifted;
                    hit.metadata
                        .insert("source".to_string(), "bridge_boost".to_string());
                    hit.metadata
                        .insert("bridge_relation".to_string(), relation.to_string());
                    hit.metadata
                        .insert("bridge_confidence".to_string(), confidence.clone());
                    chunks.push(hit);
                    added += 1;
                }
            }
        }
        tracing::info!(
            target: "bridge",
            n_entities = entities.len(),
            matched_entities,
            chunks_added = added,
            bridge_edges = index.len(),
            query_aware = !query_embedding.is_empty(),
            entities = %entities.iter().take(12).cloned().collect::<Vec<_>>().join(" | "),
            "bridge_boost ran"
        );
        added
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

        // Anchor the dominant-source expansion on the ACTUALLY-RELEVANT
        // chunks from the initial retrieval, widened with their textual
        // neighbours for narrative cohesion — instead of
        // `fetch_chunks_by_title`'s "first N chunks of the title". The title
        // fetch is sound for article-shaped sources (a Wikipedia article's
        // lead is usually what a reader wants), but for a single large
        // document chunked under ONE title — a whole book — it returns the
        // document's OPENING for any query: the Greenwich-bomb question gets
        // the shop scenes, the india-rubber-ball question never sees the
        // Professor. Centring on the real hits keeps the cohesion intent but
        // makes the fetch query-aware. Appended neighbours carry the uniform
        // cohesion score (1.0), not query similarity.
        let t_fetch = std::time::Instant::now();
        let mut by_id: std::collections::BTreeMap<u64, corpus_engine::ScoredChunk> =
            std::collections::BTreeMap::new();
        // `initial` is score-ordered; visit the dominant-source hits
        // best-first so the budget favours the most relevant regions.
        for hit in initial.iter().filter(|c| {
            (c.corpus_id.clone(), c.title.clone().unwrap_or_default()) == shape.top_source_key
        }) {
            if by_id.len() >= EXPANSION_MAX_FROM_TOP_SOURCE {
                break;
            }
            let Some(hit_id) = hit.chunk_id else { continue };
            by_id.entry(hit_id).or_insert_with(|| hit.clone());
            match idx.neighbors(hit_id, EXPANSION_NEIGHBOR_RADIUS).await {
                Ok(Some(win)) => {
                    for row in win.prev.into_iter().chain(win.next) {
                        if by_id.len() >= EXPANSION_MAX_FROM_TOP_SOURCE {
                            break;
                        }
                        by_id.entry(row.id).or_insert_with(|| {
                            // Same document as the hit → inherit its
                            // corpus/title/source ids and any other fields;
                            // overwrite only content, id, and the score.
                            let mut n = hit.clone();
                            n.content = row.content;
                            n.chunk_id = Some(row.id);
                            n.score = 1.0;
                            n
                        });
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        top_corpus_id, hit_id, error = %e,
                        "KnowledgeQuery: neighbour fetch failed — skipping this hit's window"
                    );
                }
            }
        }
        // Fallback: no chunk_id'd dominant hit in the pool (legacy index, or
        // an all-section-id pool) — keep the old title fetch so expansion
        // never silently empties.
        if by_id.is_empty() {
            match idx
                .fetch_chunks_by_title(top_title, EXPANSION_MAX_FROM_TOP_SOURCE)
                .await
            {
                Ok(fetched) => {
                    for c in fetched {
                        if let Some(id) = c.chunk_id {
                            by_id.entry(id).or_insert(c);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        top_corpus_id,
                        top_title,
                        error = %e,
                        "KnowledgeQuery: source expansion skipped — fetch_chunks_by_title failed"
                    );
                    return (initial, 0, 0, 0);
                }
            }
        }
        let fetch_ms = t_fetch.elapsed().as_millis() as u64;

        // BTreeMap iterates ascending id → natural document order. Dedupe by
        // content (re-ingestion can yield duplicate content under fresh ids).
        let mut seen_contents: HashSet<String> = HashSet::new();
        let mut expanded_dominant: Vec<corpus_engine::ScoredChunk> = Vec::new();
        for (_id, c) in by_id {
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
            let key = (c.corpus_id.clone(), c.title.clone().unwrap_or_default());
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
    /// (`/Users/user/.claude/plans/there-s-a-fast-slot-delightful-peach.md`).
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

        // Run the DeepQuery retrieval pipeline — the ordered, traced
        // step list in `retrieval_pipeline::deep_pipeline()`: the shared
        // evidence-gathering head (local ∥ mesh retrieval → scope filter
        // → store search) → the shared core (boosts, expansions, noise
        // floor, grounding, merge) → the deep tail (truncate +
        // strategy-driven top-sources expansion). The per-step trace
        // rides the `retrieval.pipeline` target. Step ORDER is
        // bench-tuned data — pinned by golden tests in
        // retrieval_pipeline.rs.
        //
        // Document-attached turns short-circuit the corpus/mesh/atlas/
        // raptor/store steps (they're routed to ComplexTask and should
        // never reach this path) but keep the historical control flow
        // of running the entity/merge tail on the empty pool.
        let mut pipeline_state = PipelineState::new(
            message,
            context,
            intent,
            scope,
            Vec::new(),
            "DeepQuery",
            format!("{intent:?}"),
        );
        if attached_source.is_some() {
            tracing::debug!("prepare_knowledge_context called with attached document — skipping (should be ComplexTask)");
        } else {
            let retrieval_query = build_retrieval_query(message, context);
            if retrieval_query != message {
                tracing::debug!(
                    bare_chars = message.len(),
                    expanded_chars = retrieval_query.len(),
                    "retrieval: expanded follow-up query with prior user turns"
                );
            }
            pipeline_state.embedding = self
                .inference
                .embed_query(&retrieval_query)
                .await
                .unwrap_or_default();
        }
        deep_pipeline(attached_source.is_none())
            .run(self, &mut pipeline_state)
            .await;
        let PipelineState {
            chunks: mut all_chunks,
            peer_attribution,
            local_hits,
            ..
        } = pipeline_state;

        // Count mesh hits that survived dedupe so the search_method
        // label reflects what's actually in the prompt.
        let mesh_hits: usize = all_chunks
            .iter()
            .filter(|c| peer_attribution.contains_key(&c.corpus_id))
            .count();

        // 4. Provenance metadata.
        let installed_corpora = self.store.list_corpus_states().await.unwrap_or_default();
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
            // Late RAPTOR injection (SOVEREIGN_RAPTOR_LATE) — see the KQ path.
            // Appended post-rerank so leaf ranking is untouched. corpus_embedding
            // is block-local to the corpus-search arm and out of scope here, so
            // re-derive the SAME query embedding (build_retrieval_query →
            // embed_query) — isolates injection TIMING, not the embedding.
            if raptor_late_inject_enabled() {
                let late_emb = self
                    .inference
                    .embed_query(&build_retrieval_query(message, context))
                    .await
                    .unwrap_or_default();
                self.apply_raptor_grounding(
                    &late_emb,
                    &mut all_chunks,
                    "DeepQuery",
                    context.conversation.enabled_corpora.as_deref(),
                )
                .await;
            }
            let conv_briefing = self
                .build_conv_briefing_block(&all_chunks, &display_categories)
                .await;
            // Phase 3 (budget-sensor redesign): mirror the KQ path's
            // ctx-aware retrieval ceiling — this path previously
            // passed EXPANDED_KNOWLEDGE_CHARS unconditionally, blind
            // to the slot's window. Reserve the response budget plus
            // last turn's REAL measured system size (memo; 4096
            // static cushion on a conversation's first turn), then
            // hand the formatter the tighter of the two caps.
            let knowledge_char_budget = {
                let mut budget = EXPANDED_KNOWLEDGE_CHARS;
                if let Some(n_ctx) = self.inference.effective_context_size() {
                    let reserved_output = self.inference_config.max_tokens as u32;
                    let system_overhead = self
                        .last_assembly(&context.conversation.id)
                        .map(|m| m.system_tokens.saturating_add(256))
                        .unwrap_or(4096);
                    let available_chars = n_ctx
                        .saturating_sub(reserved_output)
                        .saturating_sub(system_overhead)
                        .saturating_mul(4) as usize;
                    if available_chars < budget {
                        tracing::info!(
                            n_ctx,
                            reserved_output,
                            system_overhead,
                            static_budget = budget,
                            ctx_aware_budget = available_chars,
                            "deep path: ctx-aware retrieval budget tighter than static cap"
                        );
                        budget = available_chars;
                    }
                }
                budget
            };
            let doc_context = format_scored_chunks_with_kinds(
                &all_chunks,
                knowledge_char_budget,
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
            // Code-intelligence-in-chat (Inc 2): mirror the KnowledgeQuery
            // augmentation on the DEEP path. Code questions route to DeepQuery
            // (REASONING), whose synthesis evidence is assembled here — so the
            // call-graph trace must be appended at this site too, not only at
            // knowledge_query.rs. Empty string (zero overhead) for non-code
            // corpora, so it is safe to run unconditionally. Twin injection.
            let doc_context = {
                let code_trace =
                    crate::runtime::code_trace::build_code_trace_block(&all_chunks).await;
                if code_trace.is_empty() {
                    doc_context
                } else {
                    format!("{doc_context}\n\n{code_trace}")
                }
            };
            let knowledge_block = if conv_briefing.is_empty() {
                doc_context
            } else {
                format!("{conv_briefing}\n{doc_context}")
            };
            if history.is_empty() {
                format!("Relevant knowledge:\n{knowledge_block}\n\nUser: {message}\n\nAssistant:")
            } else {
                let short_history = format_history_as_prompt(context, 4);
                format!("{short_history}\n\nRelevant knowledge:\n{knowledge_block}\n\nAssistant:")
            }
        } else if history.is_empty() {
            message.to_string()
        } else {
            format!("{history}\n\nAssistant:")
        };

        // Seal audit (glassbox, ARCH §0.1/§9). When the conversation is scoped
        // to specific corpora (the `--isolate` seal / a corpus-pinned chat),
        // every retrieved chunk MUST belong to an allowed corpus (or its
        // `atlas:`-virtual / layer child). A chunk from outside the seal is a
        // cross-corpus bleed — log it loudly (with the offending corpora) so a
        // single live `--isolate` run confirms the seal holds end-to-end across
        // ALL injection paths, not just the ones audited statically.
        // `conversation-history` is exempt (prior turns, not a corpus source).
        if let Some(allow) = context.conversation.enabled_corpora.as_deref() {
            let bleed = corpora_outside_seal(&all_chunks, Some(allow));
            if bleed.is_empty() {
                tracing::info!(target: "retrieval.seal", allowed = ?allow, "DeepQuery: corpus seal intact");
            } else {
                tracing::warn!(
                    target: "retrieval.seal",
                    allowed = ?allow,
                    bleed = ?bleed,
                    "DeepQuery: cross-corpus bleed — chunks from corpora outside the conversation seal"
                );
            }
        }

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
        let budget_note =
            crate::runtime::build_response_length_directive(self.inference_config.max_tokens);
        let system = if !all_chunks.is_empty() {
            // Synthesizer role builds the prompt body (SSOT). THINKING_DIRECTIVE
            // is a `<think>`-block contract — it guides the model's HIDDEN
            // reasoning channel; include it only when a think budget is
            // allocated, else a model with no `<think>` block would execute its
            // checklist in the OPEN. DeepQuery/Simple never take the comparison
            // shape.
            let base = crate::runtime::build_synthesis_system_prompt(
                false,
                &gap_note,
                self.inference_config.think_budget > 0,
                &budget_note,
            );
            self.build_primary_system_message(&base, context)
        } else {
            self.build_system_message(
                &format!(
                    "You are a helpful AI assistant. Respond concisely and accurately.\n\n{budget_note}"
                ),
                context,
            )
        };

        // 7. Speed: the named intent→slot decision (one home for the
        // ladder; see `evidence::speed_for_retrieval_intent`).
        let speed =
            crate::runtime::evidence::speed_for_retrieval_intent(&intent, !all_chunks.is_empty());

        // 8. Build chunk summaries for frontend source linking.
        // chunk_id and source_doc_id are emitted (when present) so the
        // desktop reading surface can deref a citation back to the
        // source chunk for in-app reading + atom-graph overlay.
        let retrieved_chunks = project_retrieved_chunks(&all_chunks);

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
        if std::env::var("SOVEREIGN_COMPACTION_DISABLE")
            .ok()
            .as_deref()
            == Some("1")
        {
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

        match crate::context::summarize_dropped_history(self.inference.as_ref(), dropped).await {
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
    fn estimate_compaction_pressure(&self, context: &ConversationContext) -> (bool, u32, u32) {
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

        // Phase 2 (budget-sensor redesign): the component walk above
        // sees only history + memories + preamble — roughly a third
        // of the real prompt. System base, retrieval bundle, and the
        // response reservation are invisible to it, which is how an
        // 8k window could hard-fail at the engine while this sensor
        // read "no pressure". The assembly memo records what the LAST
        // turn's assembly actually demanded (pre-trim, including the
        // reservation); take it as a floor. First turn of a
        // conversation has no memo — one turn of the old blindness,
        // then converged.
        let real_floor = self
            .last_assembly(&context.conversation.id)
            .map(|m| m.input_tokens().saturating_add(m.reserved))
            .unwrap_or(0);
        if real_floor > total {
            tracing::debug!(
                component_estimate = total,
                real_floor,
                ctx_size,
                "compaction sensor: raising estimate to last turn's measured demand"
            );
            total = real_floor;
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
        let dropped_end = messages
            .len()
            .saturating_sub(crate::runtime::CONV_HISTORY_TURNS + 1);
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
                format!(
                    "[{:?}] {}",
                    lead.role,
                    truncate_with_ellipsis(&lead.content, 600)
                )
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
        let query_entities: std::collections::HashSet<String> =
            if let Some(g) = self.gliner.as_ref() {
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

        let jaccard =
            |a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>| -> f32 {
                if a.is_empty() || b.is_empty() {
                    return 0.0;
                }
                let inter = a.intersection(b).count() as f32;
                let union = a.union(b).count() as f32;
                if union == 0.0 {
                    0.0
                } else {
                    inter / union
                }
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

        let mut candidates: Vec<(usize, String, f32, Vec<f32>)> = scored
            .into_iter()
            .filter(|(_, _, s, _)| *s >= HISTORY_RETRIEVAL_SIM_FLOOR)
            .collect();
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
            .map(
                |(turn_index, content, similarity)| crate::types::HistoryRetrievalHit {
                    turn_index,
                    content,
                    similarity,
                },
            )
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
        let payloads = crate::memory::read_recent_tool_decisions(notes, Some(conversation_id), 32)
            .await
            .ok()?;
        let mut ids: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
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
    /// Ambient field_model: for each corpus the turn is scoped to, load its
    /// `field_skeleton.json` (System-1 enrichment) and splice a compact landscape
    /// digest into `context.knowledge_view_digests` — the same channel the
    /// system-prompt assembler renders (`system_message.rs`). This closes the
    /// "field_model is ambient for only 3 hardcoded views" gap: a turn scoped to
    /// sep / gutenberg / maple-house now gets THAT corpus's settled concerns, live
    /// tensions, and open questions ambiently, on every surface that builds this
    /// shared Runtime (bench, desktop, server) — not just the personal /
    /// conversational / institutional views the `KnowledgeViewManager` hardcodes.
    /// Because it lives in the shared runtime, bench and desktop gain it
    /// identically (the parity harness need not gate it — there's no seam to
    /// diverge).
    ///
    /// Runs AFTER `splice_landscape_digests`, so it APPENDS to (never clobbers)
    /// any view digests the provider produced. No-op when the turn is unscoped
    /// (`enabled_corpora` empty/None — we don't pay to scan every installed
    /// corpus's skeleton) or the scoped corpus has no `field_skeleton.json`.
    /// Bounded: one small JSON read + a pure render per scoped corpus.
    pub(crate) async fn splice_ambient_field_digests(&self, context: &mut ConversationContext) {
        let Some(engine) = self.corpus_engine.as_ref() else {
            return;
        };
        let corpora: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            Some(c) if !c.is_empty() => c.to_vec(),
            _ => return,
        };
        // Per-corpus token budget for the ambient digest — small + prompt-bounded,
        // matching the KnowledgeViewManager's per-view budgets (300/200).
        const FIELD_DIGEST_BUDGET_TOKENS: usize = 250;
        let mut added: Vec<crate::types::LandscapeDigest> = Vec::new();
        for corpus_id in &corpora {
            let index = match engine.open_index_for_corpus(corpus_id).await {
                Ok(idx) => idx,
                Err(e) => {
                    tracing::debug!(corpus = %corpus_id, error = %e, "ambient field_model: open_index failed");
                    continue;
                }
            };
            match index.load_field_skeleton() {
                Ok(Some(skeleton)) if !skeleton.is_empty() => {
                    let heading = format!("Field guide — {corpus_id}");
                    let body = skeleton.render_landscape(&heading, FIELD_DIGEST_BUDGET_TOKENS);
                    if !body.trim().is_empty() {
                        added.push(crate::types::LandscapeDigest {
                            view_id: format!("field:{corpus_id}"),
                            body,
                        });
                    }
                }
                Ok(_) => {} // no skeleton on disk, or empty — nothing to splice
                Err(e) => {
                    tracing::debug!(corpus = %corpus_id, error = %e, "ambient field_model: load_field_skeleton failed");
                }
            }
        }
        if added.is_empty() {
            return;
        }
        // Append to whatever `splice_landscape_digests` already set (Some on every
        // surface that wires the provider; `take().unwrap_or_default()` also keeps
        // the prompt-assembly `knowledge_view_digests.is_some()` invariant when no
        // provider ran — we always re-set Some below).
        let mut digests = context.knowledge_view_digests.take().unwrap_or_default();
        let field_count = added.len();
        digests.extend(added);
        context.set_landscape_digests(digests);
        // Glassbox: the same `retrieval_audit` channel the atom-enum /
        // atlas-grounding steps log to, so an operator can confirm the field
        // digest fired for this turn.
        tracing::info!(
            target: "retrieval_audit",
            scoped_corpora = corpora.len(),
            field_digests = field_count,
            "ambient field_model: spliced corpus field-skeleton digests"
        );
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

/// Extract the first balanced `{...}` JSON object from a string,
/// tolerating prose before and after it. Targets the Fast-slot's
/// known ramble-past-JSON failure (`{"mode":"lookup"}\n\nWait, let
/// me reconsider…`), where the whole reply is not valid JSON but the
/// leading object is. String-literal-aware so braces inside quoted
/// values don't unbalance the scan. Returns the object substring
/// (braces included) or `None` if no balanced object is present.
/// A scored RAPTOR summary candidate — the common shape the ANN-index path
/// and the brute-force scan fallback both produce before the shared
/// sort/dedupe/truncate tail in `apply_raptor_grounding`.
struct RaptorCand {
    score: f32,
    conv_uuid: String,
    corpus_id: String,
    level: i64,
    summary: String,
}

/// Build the virtual `ScoredChunk` a RAPTOR summary node injects. Shared by
/// `apply_raptor_grounding`'s ANN-index path and its brute-force scan fallback
/// so both emit byte-identical chunks. `title` is the source-doc slug (so it
/// counts toward source coverage, e.g.
/// `https://plato.stanford.edu/entries/holes/` → `holes`); `url` /
/// `source_doc_id` back-point to the origin conv_uuid; `score` is a cosine
/// similarity, so `vector_distance = 1 - score`.
fn raptor_scored_chunk(
    conv_uuid: String,
    corpus_id: String,
    level: i64,
    summary: String,
    score: f32,
) -> corpus_engine::ScoredChunk {
    let title = {
        let trimmed = conv_uuid.trim_end_matches('/');
        trimmed
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(trimmed)
            .to_string()
    };
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("source".to_string(), "raptor".to_string());
    metadata.insert("raptor_level".to_string(), level.to_string());
    corpus_engine::ScoredChunk {
        content: summary,
        title: Some(title),
        url: Some(conv_uuid.clone()),
        corpus_id,
        score,
        metadata,
        chunk_id: None,
        source_doc_id: Some(conv_uuid),
        vector_distance: Some(1.0 - score),
    }
}

fn extract_first_json_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // start and i both index ASCII bytes ('{' / '}'),
                    // so this slice never splits a UTF-8 code point.
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Build the effective [`corpus_engine::RerankConfig`] for a
/// single-corpus search. Starts from the runtime's base config (which may
/// carry an operator env-var override or a wired cross-encoder) and, when
/// the corpus's recipe declared `[retrieval] dedup_by_source` (surfaced on
/// `IndexInfo::dedup_by_source`), enables per-article source dedup for it.
///
/// This is the recipe-driven promotion of the SEP dedup lever (+6 sources,
/// 76%→85% on the eval bank, validated 2026-06-04): it now fires in every
/// runtime — desktop, server, CLI — with no env var. Corpora that don't
/// opt in are returned unchanged, so topical corpora (e.g. Wikipedia),
/// which regress under blind dedup, keep baseline behaviour.
fn rerank_config_for_corpus(
    base: &corpus_engine::RerankConfig,
    info: &corpus_engine::IndexInfo,
) -> corpus_engine::RerankConfig {
    if !info.dedup_by_source {
        return base.clone();
    }
    let mut cfg = base.clone();
    cfg.enabled = true;
    cfg.per_article = true;
    // Single-corpus search: a `None` filter means "every candidate
    // eligible", which here is exactly this (opted-in) corpus. Clearing any
    // operator-set filter avoids it excluding the very corpus that asked
    // for dedup; the cross-encoder (`rerank_fn`) is passed separately and
    // is unaffected.
    cfg.dedup_corpus_filter = None;
    cfg
}

/// Apply the per-conversation corpus allow-list to a pool of
/// `IndexInfo`. Each index passes when its `corpus_id` is in the
/// allow-list OR its `parent_corpus_id` is. The parent-aware branch
/// is what lets layer/satellite corpora (e.g. wikipedia-newsworthy
/// under wikipedia) follow their parent's enabled state without the
/// caller knowing the layer hierarchy. `None` is the no-filter
/// signal — every index passes, bit-identical to pre-feature
/// behavior.
/// Corpora present in `chunks` that fall OUTSIDE the conversation seal `allow`
/// (deduped). The read side of the isolate contract: `apply_corpus_allow_list`
/// keeps retrieval *in* the seal at fetch time; this detects any chunk that
/// nonetheless escaped it, across every injection path, for the DeepQuery
/// seal-audit trace. `conversation-history` is exempt (prior turns, not a
/// corpus source); `atlas:<corpus>` virtual chunks are checked against their
/// underlying `<corpus>`. Returns empty when `allow` is `None` (no seal) or the
/// seal holds.
fn corpora_outside_seal<'a>(
    chunks: &'a [corpus_engine::ScoredChunk],
    allow: Option<&[String]>,
) -> Vec<&'a str> {
    let Some(allow) = allow else {
        return Vec::new();
    };
    let allow_set: std::collections::HashSet<&str> = allow.iter().map(String::as_str).collect();
    chunks
        .iter()
        .map(|c| c.corpus_id.as_str())
        .filter(|cid| {
            let base = cid.strip_prefix("atlas:").unwrap_or(cid);
            *cid != "conversation-history" && !allow_set.contains(base)
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

fn apply_corpus_allow_list(
    indexes: Vec<corpus_engine::IndexInfo>,
    allow: Option<&[String]>,
) -> Vec<corpus_engine::IndexInfo> {
    let Some(allow) = allow else {
        return indexes;
    };
    let allow_set: std::collections::HashSet<&str> = allow.iter().map(String::as_str).collect();
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
    use super::rerank_config_for_corpus;
    use super::{corpora_outside_seal, raptor_scored_chunk};

    #[test]
    fn dedup_by_source_corpus_opts_into_per_article() {
        // Baseline: a corpus that did NOT declare `[retrieval]
        // dedup_by_source` is returned the runtime's base config unchanged
        // (no dedup) — preserves Wikipedia-shape behaviour.
        let base = corpus_engine::RerankConfig::default();
        assert!(!base.enabled, "precondition: base config is disabled");

        let plain = idx("wikipedia", None); // idx() sets dedup_by_source = false
        let cfg_plain = rerank_config_for_corpus(&base, &plain);
        assert!(!cfg_plain.enabled);
        assert!(!cfg_plain.per_article);

        // Opted-in corpus (SEP): per-article source dedup is enabled even
        // though the runtime base config is off and no reranker is wired.
        let mut opted = idx("sep", None);
        opted.dedup_by_source = true;
        let cfg = rerank_config_for_corpus(&base, &opted);
        assert!(cfg.enabled, "opted-in corpus enables the dedup path");
        assert!(cfg.per_article, "opted-in corpus requests per-article dedup");
        assert!(
            cfg.dedup_corpus_filter.is_none(),
            "single-corpus search clears any operator filter so this corpus is eligible"
        );
    }

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
            dedup_by_source: false,
            personal_scope: false,
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
            indexes_built: true,
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
        let pool = vec![
            idx("wikipedia", None),
            idx("sep", None),
            idx("gutenberg", None),
        ];
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

    #[test]
    fn corpora_outside_seal_flags_only_disallowed() {
        let chunks = vec![
            raptor_scored_chunk("c1".into(), "wikipedia".into(), 0, "a".into(), 0.9),
            raptor_scored_chunk("c2".into(), "sep".into(), 0, "b".into(), 0.8),
            // `atlas:sep` is a virtual chunk over the `sep` corpus.
            raptor_scored_chunk("c3".into(), "atlas:sep".into(), 0, "c".into(), 0.7),
            raptor_scored_chunk("c4".into(), "conversation-history".into(), 0, "d".into(), 0.6),
        ];
        // No seal → nothing flagged.
        assert!(corpora_outside_seal(&chunks, None).is_empty());

        // Sealed to `sep`: only `wikipedia` bleeds. `sep`, its `atlas:` virtual,
        // and conversation-history are all in-seal / exempt.
        let allow_sep = vec!["sep".to_string()];
        assert_eq!(
            corpora_outside_seal(&chunks, Some(&allow_sep)),
            vec!["wikipedia"]
        );

        // Sealed to `wikipedia`: `sep` bleeds AND `atlas:sep` bleeds (its
        // underlying corpus is outside the seal).
        let allow_wiki = vec!["wikipedia".to_string()];
        let mut bleed = corpora_outside_seal(&chunks, Some(&allow_wiki));
        bleed.sort_unstable();
        assert_eq!(bleed, vec!["atlas:sep", "sep"]);
    }

    #[test]
    fn looks_like_overview_detects_summary_questions() {
        use super::Runtime;
        // Overview/summary phrasings → true: these are the anchorless
        // "what matters here" questions that should ground on the corpus's
        // atlas Claim atoms instead of abstaining over a diffuse pool.
        for q in [
            "What is the most important thing in the maple-house material, and why?",
            "Give me an overview of this corpus.",
            "Summarize what this material is mainly about.",
            "What are the main points here?",
            "Tell me about the sep corpus.",
            "What's the gist?",
            "Give me a high-level summary.",
        ] {
            assert!(Runtime::looks_like_overview(q), "expected overview: {q:?}");
        }
        // Specific, anchored questions → false: these retrieve normally and
        // must NOT trip the claim-injection path.
        for q in [
            "What does the charter say about smoking?",
            "In what year did the Great Depression begin?",
            "What is the value of FILES_COLUMN_WIDTH?",
            "Who led the negotiation?",
        ] {
            assert!(!Runtime::looks_like_overview(q), "expected NOT overview: {q:?}");
        }
    }
}
