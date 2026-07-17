// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-cohesion expansion: dominant-source deep read and
//! additive multi-source top-up.

use super::super::*;

impl Runtime {
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
                            // Same CORPUS as the hit, but the row keeps its OWN
                            // provenance. The old "same document → inherit the
                            // anchor's title" assumption is false for
                            // row-per-document corpora (an INDEX of case files
                            // ingested as one source_doc): the positional
                            // neighbour is a DIFFERENT document, and inheriting
                            // the anchor's title mislabels its content in the
                            // synthesis prompt's [Source: …] headers, the
                            // gate's citation labels, and every citation the
                            // model copies from them (gen75 NARA
                            // misattribution: the Stevens Point row entered the
                            // evidence titled as the SAT case, so the model
                            // "misattributed" its file number faithfully).
                            let mut n = hit.clone();
                            n.content = row.content;
                            n.title = row.title.or_else(|| hit.title.clone());
                            n.url = row.url.or_else(|| hit.url.clone());
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
        // Gate-admitted structural chunks are PRIORITY grounding: they
        // carry a cross-encoder judgment (bridge-conditioned, 2026-07-17)
        // that they answer the question — a strictly stronger signal
        // than pool order. Without this, a Dominant-routed question
        // discards exactly the admissions the gate fought for
        // (measured: Einstein +4.25 admitted on synth_manhattan, then
        // evicted here; the chunk cost its displacement WITHOUT ever
        // scoring). They still count against the grounding budget.
        for c in &initial {
            let key = (c.corpus_id.clone(), c.title.clone().unwrap_or_default());
            if key == dominant_key {
                continue;
            }
            // ADDITIVE, like the truncate's raptor slots: admitted
            // chunks are ≤ PPR_MAX_ADMITTED and must not preempt the
            // grounding budget (measured: preemption displaced the
            // Copenhagen-interpretation grounding chunk — an expected
            // source — to seat a Wigner admission; −2 src net).
            if c.metadata.get("injected_by").map(|v| v == "ppr_expand").unwrap_or(false)
                && seen_contents.insert(c.content.clone())
            {
                grounding.push(c.clone());
            }
        }
        // The retained admissions must EXTEND the grounding budget,
        // not consume it — the generic loop below counts
        // `grounding.len()`, so without this offset a retained
        // admission silently evicts the last pool-order grounding
        // chunk (forensic receipt, boundary_copenhagen: admitted
        // 'Quantum mechanics' displaced the 'Copenhagen
        // interpretation' grounding chunk through three separate
        // "additive" attempts that all shared this counter).
        let retained_admitted = grounding.len();
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
            if grounding.len() < EXPANSION_GROUNDING_CHUNKS + retained_admitted
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
        message: &str,
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
            // Fetch the article WIDE and rank the top-up candidates by
            // substantive question-token overlap (2026-07-17: the
            // document-order first-N fetch made fact coverage a
            // chunk-position lottery — the 'grace' fact sat deeper in
            // 'Afterlife' than N on the comparative-religion bank
            // question, in both composition regimes). The BTree title
            // index makes the wide fetch ~ms. Document order remains
            // the DOMINANT expander's contract (narrative cohesion on
            // summarize shapes) — this is the multi-source top-up
            // only, where per-source slots are scarce and must carry
            // answer content.
            match idx
                .fetch_chunks_by_title(&key.1, EXPANSION_WIDE_FETCH)
                .await
            {
                Ok(mut group_chunks) => {
                    let q_tokens = crate::runtime::evidence::extract_tokens(
                        message,
                        crate::runtime::evidence::EVIDENCE_TITLE_MIN_TOKEN_LEN,
                    );
                    let overlap = |c: &corpus_engine::ScoredChunk| -> usize {
                        let body = c.content.to_lowercase();
                        q_tokens.iter().filter(|t| body.contains(t.as_str())).count()
                    };
                    group_chunks.sort_by_key(|c| std::cmp::Reverse(overlap(c)));
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
}
