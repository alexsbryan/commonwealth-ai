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
use super::store::{index_symbol_enrichments, symbol_source_key, IndexReport};
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
    index: Option<&CorpusIndex>,
    source_root: &Path,
    cache_dir: &Path,
    chat: &ChatCompletionFn,
    embed: &EmbedFn,
    file_filter: &[String],
) -> Result<CodeIntelReport> {
    let sources = enumerate_symbol_sources(scip, source_root, file_filter).await?;
    // Inc 4 polish: test functions aren't "how does this code work" targets — they
    // crowd the retrieval set ("tests", "*_nonempty", ...) and carry no useful
    // call-graph answer. Two test shapes, two SCIP encodings:
    //   - integration tests + their helpers/fixtures under `<crate>/tests/foo.rs`
    //     — the SCIP qualified_name is the BARE fn name (no path segment), so they
    //     ONLY show up in `file_path`. The prior check looked solely at
    //     qualified_name and leaked ~2,300 of these (8% of the enriched set,
    //     verified on commonwealth-ai 2026-06-27) into enrichment.
    //   - inline `#[cfg(test)] mod tests` — the SCIP descriptor carries a `/tests/`
    //     segment, which the qualified_name check catches.
    // Drop both from enrichment AND prune any summary a prior pass already indexed,
    // so the code-intel chunk set stays code-not-tests (re-runs converge).
    let (sources, test_sources): (Vec<_>, Vec<_>) = sources.into_iter().partition(|s| {
        let m = &s.meta;
        !(m.file_path.contains("/tests/")
            || m.file_path.starts_with("tests/")
            || m.qualified_name.contains("/tests/"))
    });
    for t in &test_sources {
        if let Some(idx) = index {
            let _ = idx
                .delete_chunks_by_source_doc(&symbol_source_key(&t.meta))
                .await;
        }
    }
    if !test_sources.is_empty() {
        tracing::info!(
            target: "enrichment.code_intel",
            test_symbols = test_sources.len(),
            "code_intel: dropped test functions + pruned any prior summaries",
        );
    }
    let symbols = sources.len();

    // Chunked, checkpointing pass. The whole-corpus run is ~22k functions / many
    // hours serially; saving the cache + index only at the very end would make a
    // single crash cost the entire run. Instead we enrich in batches and, after
    // each batch, index it and rewrite the cache with everything done so far — so
    // a re-run reuses every summary already produced (body-hash gated) and only
    // the un-done tail repeats. `reuse_pool` carries prior + completed-this-run,
    // so a body summarized in an earlier batch (or a prior run) is never redone.
    const CHECKPOINT_CHUNK: usize = 200;
    let prior = load_cache(cache_dir);
    let mut reuse_pool = prior;
    // Seed `done` with the prior cache so every checkpoint rewrites the FULL cache
    // (prior + completed-this-run), never just this run's output — otherwise the
    // first checkpoint would overwrite the prior slice on disk and a crash before
    // a slice fn's chunk ran would lose it. This also makes a scoped (`--files`)
    // run merge into the cache instead of replacing it.
    let mut done: HashMap<String, SymbolEnrichment> = reuse_pool.clone();
    let mut enrich = IncrementalReport {
        total: symbols,
        ..Default::default()
    };
    let mut index_rep = IndexReport::default();
    let total_chunks = sources.len().div_ceil(CHECKPOINT_CHUNK).max(1);

    for (i, chunk) in sources.chunks(CHECKPOINT_CHUNK).enumerate() {
        let (chunk_out, rep) =
            enrich_symbols_incremental(chat, chunk.to_vec(), &reuse_pool).await;
        // Cache-only mode (`index` is None when SOVEREIGN_ENRICH_SKIP_INDEX is
        // set): produce just the body-hash cache (code_intel_cache.json) — the
        // sole artifact capability-doc / reconcile read — and never open or write
        // chunks.lance. A live daemon co-managing the index storms it on external
        // access, so cache-only is the conflict-free path when the daemon stays up.
        let irep = match index {
            Some(idx) => index_symbol_enrichments(idx, embed, &chunk_out).await,
            None => IndexReport::default(),
        };
        for e in &chunk_out {
            reuse_pool.insert(e.body_hash.clone(), e.clone());
            done.insert(e.body_hash.clone(), e.clone());
        }
        let snapshot: Vec<SymbolEnrichment> = done.values().cloned().collect();
        save_cache(cache_dir, &snapshot)?;
        enrich.reused += rep.reused;
        enrich.regenerated += rep.regenerated;
        enrich.failed += rep.failed;
        index_rep.upserted += irep.upserted;
        index_rep.skipped += irep.skipped;
        index_rep.failed += irep.failed;
        tracing::info!(
            target: "enrichment.code_intel",
            chunk = i + 1,
            total_chunks,
            done = done.len(),
            regenerated = enrich.regenerated,
            failed = enrich.failed,
            "code_intel: checkpoint saved",
        );
    }

    let report = CodeIntelReport {
        symbols,
        enrich,
        index: index_rep,
    };
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
    let t = std::time::Instant::now();
    let scip = ScipGraph::open(&corpus_dir.join("scip_graph.db"), corpus_id)?;
    tracing::info!(target: "enrichment.code_intel", ms = t.elapsed().as_millis() as u64, "open: scip_graph");
    let index_owned = if std::env::var("SOVEREIGN_ENRICH_SKIP_INDEX").is_ok() {
        tracing::info!(target: "enrichment.code_intel", "SKIP_INDEX: not opening chunks.lance — cache-only pass");
        None
    } else {
        let t = std::time::Instant::now();
        let idx = CorpusIndex::open(corpus_dir).await?;
        tracing::info!(target: "enrichment.code_intel", ms = t.elapsed().as_millis() as u64, "open: corpus_index");
        Some(idx)
    };
    run_code_intel(&scip, index_owned.as_ref(), &source_root, corpus_dir, chat, embed, file_filter).await
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
        let rep = run_code_intel(&scip, Some(&index), src.path(), cache.path(), &chat, &embed, &[])
            .await
            .unwrap();
        assert_eq!(rep.symbols, 1);
        assert_eq!(rep.enrich.regenerated, 1, "summarized once");
        assert_eq!(rep.index.upserted, 1, "indexed once");
        assert_eq!(index.chunk_count().await.unwrap(), 1);

        // Second pass: unchanged body -> cache hit (no LLM) + index skip (no write).
        let rep2 = run_code_intel(&scip, Some(&index), src.path(), cache.path(), &chat, &embed, &[])
            .await
            .unwrap();
        assert_eq!(rep2.enrich.reused, 1, "summary reused from the body-hash cache");
        assert_eq!(rep2.enrich.regenerated, 0, "no model call for an unchanged body");
        assert_eq!(rep2.index.skipped, 1, "chunk unchanged");
        assert_eq!(index.chunk_count().await.unwrap(), 1, "no new rows");
    }
}
