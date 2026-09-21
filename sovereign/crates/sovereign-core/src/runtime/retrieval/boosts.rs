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
        lane: &crate::runtime::Lane,
    ) -> Vec<MetaAtlasHitRecord> {
        // Clone the `Arc` out and drop the guard before the awaits below
        // (`index` is consulted across them; a std `RwLock` guard is not
        // `Send`). `None` until the desktop's deferred warm attaches the
        // index — boost simply short-circuits until then.
        let index = lane.meta_atlas.clone();
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
                        lane,
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

}
