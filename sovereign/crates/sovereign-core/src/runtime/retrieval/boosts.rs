// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entity-anchored boosts: meta-atlas canonical-entity
//! injection + cross-corpus bridge boost.

use super::super::*;

impl Runtime {
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
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "on" | "true" | "yes"
                )
            })
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
}
