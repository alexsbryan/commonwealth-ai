//! Atlas-grounded retrieval primitives shared between the eval CLI
//! and the runtime chat path.
//!
//! The atlas is a typed knowledge graph computed offline (see
//! `corpus-engine/ATLAS.md`). At query time, retrieval can fuse atlas
//! Entity matches into the chunk hit set as virtual `ScoredChunk`s:
//! cosine the question embedding against pre-embedded Entity
//! descriptions, take top-K, surface them as additional candidates.
//! This module owns the data types + math; the eval CLI provides one
//! loader (against `ChatSession::inference`) and the daemon provides
//! another (`sovereign-tools::atlas_context_manager`) that loads at
//! daemon boot and reuses across queries.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::ScoredChunk;

/// One pre-embedded atlas Entity available to retrieval as a virtual
/// chunk. Built by a loader, immutable after that.
#[derive(Debug, Clone)]
pub struct AtlasEntry {
    pub canonical_name: String,
    pub embed_text: String,
    pub embedding: Vec<f32>,
}

/// Pre-embedded atlas entity bag for one corpus. Carries the
/// `top_k` the loader was constructed with so the per-query call
/// site doesn't need to re-pick a value.
#[derive(Debug, Clone)]
pub struct AtlasContext {
    pub atlas_corpus_id: String,
    pub entries: Vec<AtlasEntry>,
    pub top_k: usize,
}

/// Cosine similarity. Returns 0 on zero-length vectors or
/// dimension mismatch — both are signs of a misconfigured loader,
/// and silently degrading to zero score keeps retrieval going
/// rather than poisoning a query.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
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
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

/// Score every entry by cosine sim to `query_embedding`, take the
/// top-K from `ctx`, return as virtual `ScoredChunk`s. Each chunk's
/// `corpus_id` is `atlas:<corpus_id>` so downstream provenance keeps
/// the origin obvious — the per-question report distinguishes
/// "wikipedia chunk" from "atlas-derived virtual chunk."
///
/// Phase C4 — every chunk also carries provenance metadata so eval
/// `--inspect` and the desktop's hit attribution can surface where
/// each result actually came from:
///
///   - `metadata["source"] = "atlas"` — discriminator for atlas vs
///     chunk vs mesh-peer hits.
///   - `metadata["atlas_corpus"] = <corpus_id>` — the underlying
///     corpus the atlas was built over.
///   - `metadata["atlas_tier"] = "tier-2"` — for now we only carry
///     extracted entries (see `AtlasContextFilter::default`); a
///     future per-entry tier would land here when the loader
///     surfaces mixed depths.
pub fn atlas_top_k_as_chunks(
    query_embedding: &[f32],
    ctx: &AtlasContext,
) -> Vec<ScoredChunk> {
    let mut scored: Vec<(f32, &AtlasEntry)> = ctx
        .entries
        .iter()
        .map(|e| (cosine(query_embedding, &e.embedding), e))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(ctx.top_k);
    scored
        .into_iter()
        .map(|(score, e)| {
            let mut metadata = HashMap::new();
            metadata.insert("source".to_string(), "atlas".to_string());
            metadata.insert("atlas_corpus".to_string(), ctx.atlas_corpus_id.clone());
            metadata.insert("atlas_tier".to_string(), "tier-2".to_string());
            ScoredChunk {
                content: e.embed_text.clone(),
                title: Some(e.canonical_name.clone()),
                url: None,
                corpus_id: format!("atlas:{}", ctx.atlas_corpus_id),
                score,
                metadata,
                chunk_id: None,
                source_doc_id: None,
                vector_distance: None,
            }
        })
        .collect()
}

/// Source of `AtlasContext`s, looked up at query time. The runtime
/// holds an `Option<Arc<dyn AtlasContextProvider>>` and consults it
/// inside the chunk-retrieval path; the daemon's
/// `AtlasContextManager` is the production implementation, while
/// the eval CLI builds one inline from `ChatSession`.
pub trait AtlasContextProvider: Send + Sync {
    /// Look up a pre-loaded context by its atlas corpus id. Returns
    /// `None` when no atlas has been loaded for that id (e.g. the
    /// corpus has no `atlas/` dir, or daemon boot is still warming).
    fn get(&self, atlas_corpus_id: &str) -> Option<Arc<AtlasContext>>;

    /// All atlas corpus ids currently loaded. Used by the runtime
    /// to fuse atlas grounding for every installed corpus that has
    /// one — the caller doesn't need to know which corpora have
    /// atlases ahead of time.
    fn loaded_corpus_ids(&self) -> Vec<String>;

    /// Record that `canonical_name` from `atlas_corpus_id` matched a
    /// query (i.e. it landed in the top-K returned by
    /// [`atlas_top_k_as_chunks`]). Persisted as a per-corpus bump
    /// map and consumed by the next triage rebuild as a centrality
    /// addition — articles users actually ask about move up the
    /// Tier-2 enrichment queue. Default: no-op (eval CLI doesn't
    /// need adaptive triage).
    fn record_match(&self, _atlas_corpus_id: &str, _canonical_name: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, embed: Vec<f32>) -> AtlasEntry {
        AtlasEntry {
            canonical_name: name.to_string(),
            embed_text: format!("{name} desc"),
            embedding: embed,
        }
    }

    #[test]
    fn cosine_matches_identical_vector_at_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine(&v, &v);
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_on_dim_mismatch() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn top_k_returns_highest_cosine_first() {
        let ctx = AtlasContext {
            atlas_corpus_id: "test".into(),
            entries: vec![
                entry("Far", vec![-1.0, -1.0]),
                entry("Near", vec![1.0, 1.0]),
                entry("Mid", vec![1.0, 0.0]),
            ],
            top_k: 2,
        };
        let q = vec![1.0, 1.0];
        let chunks = atlas_top_k_as_chunks(&q, &ctx);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].title.as_deref(), Some("Near"));
        assert_eq!(chunks[0].corpus_id, "atlas:test");
    }

    /// Phase C4: every atlas chunk carries provenance metadata so
    /// downstream consumers can distinguish atlas vs chunk vs mesh
    /// hits without sniffing the corpus_id prefix.
    #[test]
    fn atlas_chunks_carry_provenance_metadata() {
        let ctx = AtlasContext {
            atlas_corpus_id: "wikipedia".into(),
            entries: vec![entry("Earth", vec![1.0, 0.0])],
            top_k: 1,
        };
        let chunks = atlas_top_k_as_chunks(&[1.0, 0.0], &ctx);
        let m = &chunks[0].metadata;
        assert_eq!(m.get("source").map(|s| s.as_str()), Some("atlas"));
        assert_eq!(m.get("atlas_corpus").map(|s| s.as_str()), Some("wikipedia"));
        assert_eq!(m.get("atlas_tier").map(|s| s.as_str()), Some("tier-2"));
    }
}
