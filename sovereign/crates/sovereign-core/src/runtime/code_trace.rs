// SPDX-License-Identifier: AGPL-3.0-or-later
//! Code-intelligence-in-chat — the runtime evidence augmentation (Inc 2 slice 2b).
//!
//! When retrieval surfaces a code-intel *summary* chunk (written by
//! `corpus_engine::enrichment::code_intel::store`, which tags every such chunk
//! `source = "code_intel_summary"` in its metadata JSON), this module opens the
//! owning corpus's `scip_graph.db` and renders a 1-hop caller/callee trace for
//! the matched symbol. The knowledge-query handler appends that block to the
//! synthesis evidence so the model can answer "how does X connect to Y" with the
//! real call graph rather than guessing from prose.
//!
//! The chat answer path is single-shot retrieve -> synthesize, so this is a
//! *deterministic* augmentation, NOT an agentic tool-loop.
//!
//! Lean by construction: depends only on `corpus-engine-scip` (a SQLite reader
//! over `scip_graph.db`), never on the tree-sitter grammars that BUILT the
//! graph. That read/write split is the whole point — the chat runtime ships in
//! every binary, and dragging 5 grammar crates into it to *read* a graph would
//! be backwards. The grammars stay confined to the indexing path that writes the
//! db (the CLI `enrich`/`code index` verbs and the daemon watcher).
//!
//! Best-effort throughout: a missing/corrupt db, a corpus with no graph, or a
//! symbol with no recorded edges all yield no trace and never disturb the answer.

use std::path::PathBuf;

use corpus_engine::ScoredChunk;
use corpus_engine_scip::{build_symbol_trace, render_trace, ScipGraph};

/// Metadata marker `code_intel::store::insert_chunk_for` stamps on every
/// summary chunk. The per-chunk detector — precise (only actual summary chunks
/// get traced) and independent of whether the corpus is tagged `CorpusKind::Code`.
const CODE_INTEL_SOURCE: &str = "code_intel_summary";

/// Cap on distinct symbols traced per turn. Bounds prompt cost and reflects the
/// §5 hardening finding "never trust retrieval rank-1": we attach traces for the
/// top few code hits as evidence (letting the model + graph disambiguate),
/// rather than betting the answer on chunk #1 alone.
const MAX_TRACED_SYMBOLS: usize = 3;

/// A code-intel hit distilled from one retrieved chunk's metadata.
struct CodeHit {
    corpus_id: String,
    /// Short symbol name — the key `find_callers` matches on.
    symbol: String,
    /// SCIP descriptor — the key `find_callees_qualified` needs (may be empty).
    qualified_name: String,
}

/// Resolve `<data>/indexes/<corpus_id>/scip_graph.db`, matching the daemon's
/// writer (`sovereign-cli-dev/project_cmd.rs`) and the atlas reader
/// (`runtime/evidence_loop.rs`) — one canonical layout, three readers.
fn scip_db_path(corpus_id: &str) -> PathBuf {
    let base = std::env::var("SOVEREIGN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    base.join("indexes").join(corpus_id).join("scip_graph.db")
}

/// Distill the distinct code-intel summary hits from a retrieved chunk set, in
/// retrieval order, deduped by qualified name (falling back to symbol), capped
/// at [`MAX_TRACED_SYMBOLS`]. Pure — unit-tested without a graph.
fn code_hits(chunks: &[ScoredChunk]) -> Vec<CodeHit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    for c in chunks {
        if c.metadata.get("source").map(String::as_str) != Some(CODE_INTEL_SOURCE) {
            continue;
        }
        let symbol = c.metadata.get("symbol").cloned().unwrap_or_default();
        if symbol.is_empty() {
            continue;
        }
        let qualified_name = c
            .metadata
            .get("qualified_name")
            .cloned()
            .unwrap_or_default();
        let key = if qualified_name.is_empty() {
            symbol.clone()
        } else {
            qualified_name.clone()
        };
        if !seen.insert(key) {
            continue;
        }
        hits.push(CodeHit {
            corpus_id: c.corpus_id.clone(),
            symbol,
            qualified_name,
        });
        if hits.len() >= MAX_TRACED_SYMBOLS {
            break;
        }
    }
    hits
}

/// Group hits by corpus, preserving first-seen order, so each `scip_graph.db` is
/// opened exactly once even when several traced symbols share a corpus.
fn group_by_corpus(hits: Vec<CodeHit>) -> Vec<(String, Vec<CodeHit>)> {
    let mut grouped: Vec<(String, Vec<CodeHit>)> = Vec::new();
    for hit in hits {
        if let Some((_, bucket)) = grouped.iter_mut().find(|(c, _)| *c == hit.corpus_id) {
            bucket.push(hit);
        } else {
            grouped.push((hit.corpus_id.clone(), vec![hit]));
        }
    }
    grouped
}

/// Build the call-graph evidence block for any code-intel summary chunks in the
/// retrieved set. Returns an empty string when there are none — zero overhead
/// for the common (non-code) corpus, so it is safe to call unconditionally.
pub async fn build_code_trace_block(chunks: &[ScoredChunk]) -> String {
    let hits = code_hits(chunks);
    if hits.is_empty() {
        return String::new();
    }

    let mut blocks = Vec::new();
    for (corpus_id, hits) in group_by_corpus(hits) {
        let path = scip_db_path(&corpus_id);
        if !path.exists() {
            tracing::debug!(
                target: "runtime.code_trace",
                corpus = %corpus_id,
                "code-intel hit but no scip_graph.db; skipping trace",
            );
            continue;
        }
        let graph = match ScipGraph::open(&path, &corpus_id) {
            Ok(g) => g,
            Err(e) => {
                tracing::debug!(
                    target: "runtime.code_trace",
                    corpus = %corpus_id,
                    error = %e,
                    "scip_graph.db open failed; skipping trace",
                );
                continue;
            }
        };
        for hit in hits {
            match build_symbol_trace(&graph, &hit.symbol, &hit.qualified_name).await {
                Ok(trace) => blocks.push(render_trace(&trace)),
                Err(e) => tracing::debug!(
                    target: "runtime.code_trace",
                    symbol = %hit.symbol,
                    error = %e,
                    "build_symbol_trace failed; skipping",
                ),
            }
        }
    }

    if blocks.is_empty() {
        return String::new();
    }
    // Glassbox: operators can see the feature fire and how many symbols it traced.
    tracing::info!(
        target: "runtime.code_trace",
        traced = blocks.len(),
        "injected call-graph trace into synthesis evidence",
    );
    let mut out = String::from(
        "CODE CALL-GRAPH (resolved from the indexed source — use it to trace how the \
         code connects; a `dyn-dispatch` marker is a trait/dynamic boundary where a \
         text search would miss the link):\n\n",
    );
    out.push_str(&blocks.join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn chunk(corpus: &str, meta: &[(&str, &str)]) -> ScoredChunk {
        let metadata: HashMap<String, String> = meta
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ScoredChunk {
            content: "summary text".to_string(),
            title: None,
            url: None,
            corpus_id: corpus.to_string(),
            score: 1.0,
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn only_code_intel_summary_chunks_become_hits() {
        let chunks = vec![
            // A normal prose chunk — ignored.
            chunk("wiki", &[("source", "document")]),
            // A code-intel summary — picked up.
            chunk(
                "codecorpus",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", "route_query"),
                    ("qualified_name", "crate::router::route_query"),
                ],
            ),
        ];
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol, "route_query");
        assert_eq!(hits[0].qualified_name, "crate::router::route_query");
        assert_eq!(hits[0].corpus_id, "codecorpus");
    }

    #[test]
    fn dedups_by_qualified_name_and_caps() {
        // Same symbol twice (two chunks for one symbol) collapses to one hit;
        // four distinct symbols cap at MAX_TRACED_SYMBOLS.
        let mut chunks = vec![
            chunk("c", &[("source", "code_intel_summary"), ("symbol", "a"), ("qualified_name", "crate::a")]),
            chunk("c", &[("source", "code_intel_summary"), ("symbol", "a"), ("qualified_name", "crate::a")]),
        ];
        for name in ["b", "d", "e", "f"] {
            chunks.push(chunk(
                "c",
                &[
                    ("source", "code_intel_summary"),
                    ("symbol", name),
                    ("qualified_name", "crate::x"),
                ],
            ));
        }
        // Note: b/d/e/f share qualified_name "crate::x" so they dedup to ONE; a
        // is distinct -> 2 distinct keys total, under the cap.
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), 2, "deduped by qualified_name");
        assert_eq!(hits[0].symbol, "a");
    }

    #[test]
    fn caps_at_max_traced_symbols() {
        // Ten chunks with distinct qualified names -> ten distinct hits, capped.
        let chunks: Vec<ScoredChunk> = (0..10)
            .map(|i| {
                let metadata: HashMap<String, String> = [
                    ("source".to_string(), "code_intel_summary".to_string()),
                    ("symbol".to_string(), format!("s{i}")),
                    ("qualified_name".to_string(), format!("crate::s{i}")),
                ]
                .into_iter()
                .collect();
                ScoredChunk {
                    content: String::new(),
                    title: None,
                    url: None,
                    corpus_id: "c".to_string(),
                    score: 1.0,
                    metadata,
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                }
            })
            .collect();
        let hits = code_hits(&chunks);
        assert_eq!(hits.len(), MAX_TRACED_SYMBOLS);
    }

    #[tokio::test]
    async fn empty_when_no_code_chunks() {
        let chunks = vec![chunk("wiki", &[("source", "document")])];
        assert!(build_code_trace_block(&chunks).await.is_empty());
    }
}
