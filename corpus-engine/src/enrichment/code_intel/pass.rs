// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code-intel enrichment pass (slice 4) — composes slices 1-3 into one
//! entry point: SCIP-enumerate symbols -> incrementally summarize the changed
//! bodies -> upsert the summaries as searchable chunks.
//!
//! Patchability is end-to-end: a body-hash sidecar cache (`code_intel_cache.json`)
//! lets [`run_code_intel`] skip the LLM for unchanged bodies (slice 1's gate),
//! and the storage layer skips the re-embed + write for unchanged bodies (slice
//! 3's gate). Per-commit cost is the number of changed function bodies (§3.4).
//!
//! Gated on `treesitter` (pulls in `corpus-engine-scip`).

use std::collections::HashMap;
use std::path::Path;

use corpus_engine_scip::ScipGraph;

use crate::enrichment::pipeline::types::ChatCompletionFn;
use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::types::EmbedFn;

use super::scip_source::enumerate_symbol_sources;
use super::store::{index_symbol_enrichments, IndexReport};
use super::{enrich_symbols_incremental, IncrementalReport, SymbolEnrichment};

/// Sidecar that carries prior summaries across runs (keyed by body-hash) so an
/// unchanged body never re-hits the model. Lives beside the corpus index.
const CACHE_FILE: &str = "code_intel_cache.json";

/// Combined glassbox report for one pass.
#[derive(Debug, Default, Clone)]
pub struct CodeIntelReport {
    pub symbols: usize,
    pub enrich: IncrementalReport,
    pub index: IndexReport,
}

fn load_cache(cache_dir: &Path) -> HashMap<String, SymbolEnrichment> {
    match std::fs::read_to_string(cache_dir.join(CACHE_FILE)) {
        Ok(s) => serde_json::from_str::<Vec<SymbolEnrichment>>(&s)
            .map(|v| v.into_iter().map(|e| (e.body_hash.clone(), e)).collect())
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_cache(cache_dir: &Path, enrichments: &[SymbolEnrichment]) -> Result<()> {
    let json = serde_json::to_string(enrichments)
        .map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::write(cache_dir.join(CACHE_FILE), json)?;
    Ok(())
}

/// Run the code-intel enrichment pass against an already-open SCIP graph + index
/// (dependency-injected so this is testable with in-memory fixtures). Reads the
/// body-hash cache from `cache_dir`, (re-)summarizes only changed bodies, upserts
/// the summaries, then rewrites the cache.
pub async fn run_code_intel(
    scip: &ScipGraph,
    index: &CorpusIndex,
    source_root: &Path,
    cache_dir: &Path,
    chat: &ChatCompletionFn,
    embed: &EmbedFn,
    file_filter: &[String],
) -> Result<CodeIntelReport> {
    let sources = enumerate_symbol_sources(scip, source_root, file_filter).await?;
    let symbols = sources.len();

    let prior = load_cache(cache_dir);
    let (enrichments, enrich) = enrich_symbols_incremental(chat, sources, &prior).await;

    let index = index_symbol_enrichments(index, embed, &enrichments).await;

    save_cache(cache_dir, &enrichments)?;

    let report = CodeIntelReport { symbols, enrich, index };
    tracing::info!(
        target: "enrichment.code_intel",
        symbols = report.symbols,
        regenerated = report.enrich.regenerated,
        reused = report.enrich.reused,
        upserted = report.index.upserted,
        skipped = report.index.skipped,
        "code_intel: pass complete",
    );
    Ok(report)
}

/// Open a corpus's SCIP graph + index by path and run the pass. `corpus_dir`
/// holds `scip_graph.db`, the `chunks.lance` index, the cache, and the
/// `_corpus_meta.json` from which the original source tree is resolved. This is
/// the entry point a CLI verb or pipeline hook calls.
pub async fn run_code_intel_for_corpus(
    corpus_dir: &Path,
    corpus_id: &str,
    chat: &ChatCompletionFn,
    embed: &EmbedFn,
    file_filter: &[String],
) -> Result<CodeIntelReport> {
    let source_root = corpus_source_root(corpus_dir)?;
    let scip = ScipGraph::open(&corpus_dir.join("scip_graph.db"), corpus_id)?;
    let index = CorpusIndex::open(corpus_dir).await?;
    run_code_intel(&scip, &index, &source_root, corpus_dir, chat, embed, file_filter).await
}

/// Resolve the corpus's original source tree from its `_corpus_meta.json`
/// (`source_path` field), so symbol bodies can be read from disk. Mirrors
/// `atlas::code_walk::read_source_metadata` but reads the corpus's own dir.
fn corpus_source_root(corpus_dir: &Path) -> Result<std::path::PathBuf> {
    let raw = std::fs::read_to_string(corpus_dir.join("_corpus_meta.json"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| Error::Serialization(e.to_string()))?;
    v.get("source_path")
        .and_then(|s| s.as_str())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "code_intel: corpus at {} has no `source_path` in _corpus_meta.json",
                corpus_dir.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::types::{ChatCompletionFn, ChatPrompt};
    use crate::index::CorpusIndex;
    use crate::types::EmbedFn;
    use corpus_engine_scip::{ScipGraph, ScipSymbolRecord};
    use std::sync::Arc;

    fn fn_symbol(name: &str, file: &str, ls: i32, le: i32) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: name.to_string(),
            qualified_name: format!("crate::{name}"),
            kind: "function".to_string(),
            file_path: file.to_string(),
            line_start: ls,
            line_end: le,
            language: "rust".to_string(),
        }
    }

    fn fake_chat() -> ChatCompletionFn {
        Arc::new(|_p: &ChatPrompt| {
            Box::pin(async {
                Ok("SUMMARY: it routes and runs the request.\nASKS: where does it go? what runs it?"
                    .to_string())
            })
        })
    }

    fn fake_embed() -> EmbedFn {
        Arc::new(|_s: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
    }

    #[tokio::test]
    async fn full_pass_summarizes_indexes_and_is_patchable() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("z.rs"),
            "fn handle() {\n    route_and_run_the_request();\n}\n",
        )
        .unwrap();

        let scip = ScipGraph::open_in_memory("c").unwrap();
        scip.ingest_symbols_and_refs(vec![fn_symbol("handle", "z.rs", 0, 2)], vec![])
            .await
            .unwrap();

        let idxdir = tempfile::tempdir().unwrap();
        let index =
            CorpusIndex::create(&idxdir.path().join("c"), "c", "C", "test-model", 4, false, "MIT")
                .await
                .unwrap();

        let cache = tempfile::tempdir().unwrap();
        let (chat, embed) = (fake_chat(), fake_embed());

        // First pass: one symbol, summarized + indexed.
        let rep = run_code_intel(&scip, &index, src.path(), cache.path(), &chat, &embed, &[])
            .await
            .unwrap();
        assert_eq!(rep.symbols, 1);
        assert_eq!(rep.enrich.regenerated, 1, "summarized once");
        assert_eq!(rep.index.upserted, 1, "indexed once");
        assert_eq!(index.chunk_count().await.unwrap(), 1);

        // Second pass: unchanged body -> cache hit (no LLM) + index skip (no write).
        let rep2 = run_code_intel(&scip, &index, src.path(), cache.path(), &chat, &embed, &[])
            .await
            .unwrap();
        assert_eq!(rep2.enrich.reused, 1, "summary reused from the body-hash cache");
        assert_eq!(rep2.enrich.regenerated, 0, "no model call for an unchanged body");
        assert_eq!(rep2.index.skipped, 1, "chunk unchanged");
        assert_eq!(index.chunk_count().await.unwrap(), 1, "no new rows");
    }
}
