// SPDX-License-Identifier: AGPL-3.0-or-later
//! Conversation-tiered retrieval surfaces: PPR entity-graph
//! rerank of conv-corpus chunks + the tiered briefing block.

use super::super::*;

impl Runtime {
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
}
