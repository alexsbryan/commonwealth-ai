// SPDX-License-Identifier: AGPL-3.0-or-later
//! RAPTOR collapsed-tree grounding: ANN summary-index
//! candidates with brute-force scan fallback, injected as
//! virtual summary chunks.

use super::super::*;

impl Runtime {
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
                node_id: h.node_id,
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
                    node_id: node.node_id,
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
                c.node_id,
            ));
        }
        tracing::info!(
            label,
            added,
            top_m,
            min_level,
            via_index,
            via_scan,
            "raptor-grounding: collapsed-tree summaries injected"
        );
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
    /// `RaptorNode.node_id` — the provenance handle. Both candidate
    /// paths already hold it (`RaptorHit.node_id`; the scan's
    /// `ConvRaptorNodeRow.node_id`); dropping it here was the missing
    /// link ECONOMY §7.8 names for `quote_spans` carriage. Threaded
    /// into the injected chunk's `raptor_node_id` metadata so any
    /// downstream consumer (citation UI, the judge's summary-claim
    /// clearing) can resolve summary → node → `quote_spans` /
    /// `evidence_chunk_ids` without re-searching.
    node_id: String,
}

/// Build the virtual `ScoredChunk` a RAPTOR summary node injects. Shared by
/// `apply_raptor_grounding`'s ANN-index path and its brute-force scan fallback
/// so both emit byte-identical chunks. `title` is the source-doc slug (so it
/// counts toward source coverage, e.g.
/// `https://plato.stanford.edu/entries/holes/` → `holes`); `url` /
/// `source_doc_id` back-point to the origin conv_uuid; `score` is a cosine
/// similarity, so `vector_distance = 1 - score`.
pub(super) fn raptor_scored_chunk(
    conv_uuid: String,
    corpus_id: String,
    level: i64,
    summary: String,
    score: f32,
    node_id: String,
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
    // Provenance handle (ECONOMY §7.8's missing link): resolves this
    // virtual chunk back to its `conv_raptor_nodes` row, whose
    // `quote_spans` + `evidence_chunk_ids` are the verbatim,
    // leaf-traceable surface behind the summary. No gate site reads
    // this key today; it is the substrate the judge-side summary-claim
    // clearing (replay-sibling work) and the citation UI consume.
    if !node_id.is_empty() {
        metadata.insert("raptor_node_id".to_string(), node_id);
    }
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

#[cfg(test)]
mod raptor_chunk_tests {
    use super::raptor_scored_chunk;

    /// The provenance thread (ECONOMY §7.8's missing link): the
    /// injected virtual chunk must carry its `RaptorNode.node_id` so
    /// downstream consumers can resolve summary → node →
    /// `quote_spans` / `evidence_chunk_ids`. Failing input: before
    /// 2026-08-14 both candidate paths held the node_id and dropped
    /// it at `RaptorCand`, leaving summary provenance unresolvable.
    #[test]
    fn injected_chunk_carries_node_id_provenance() {
        let c = raptor_scored_chunk(
            "https://plato.stanford.edu/entries/compatibilism/".to_string(),
            "sep".to_string(),
            1,
            "A summary.".to_string(),
            0.9,
            "node-abc-123".to_string(),
        );
        assert_eq!(
            c.metadata.get("raptor_node_id").map(String::as_str),
            Some("node-abc-123")
        );
        assert_eq!(c.metadata.get("source").map(String::as_str), Some("raptor"));
        // The gate's Leaf/Summary split and the formatter's tier
        // branch both key on `source == "raptor"`; the node_id must
        // not disturb that marker.
        assert_eq!(c.chunk_id, None);
    }

    /// An empty node_id (defensive: a store row predating the column)
    /// must not insert an empty-string handle a consumer could
    /// mistake for a real id.
    #[test]
    fn empty_node_id_is_omitted_not_inserted() {
        let c = raptor_scored_chunk(
            "conv/1".to_string(),
            "sep".to_string(),
            0,
            "s".to_string(),
            0.5,
            String::new(),
        );
        assert!(!c.metadata.contains_key("raptor_node_id"));
    }
}
