// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod gutenberg;
pub mod html_crawl;
pub mod manager;
pub mod openalex;
pub mod parquet_reader;
pub mod registry;
pub mod stackexchange;
pub mod wikipedia;

use std::path::Path;
use std::sync::Arc;

use sovereign_core::error::Result;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{DocumentChunk, SourceType};

use crate::rag::chunk::chunk_text;

// The one `strip_html` (§10.6). This module carried a hand-copy until
// 2026-08-20; the two drifted and the copy here silently truncated every
// document at its first `</script>` — see the regression test at the bottom of
// this file. Re-exported at the historical path so `html_crawl`,
// `stackexchange` and `sec_edgar` call sites are unchanged.
pub(crate) use corpus_engine::extractors::strip_html;

pub use manager::{CorpusInstallPhase, CorpusManager, CorpusProgress, ProgressCallback};
pub use registry::{CorpusDefinition, CorpusRegistry, TierDefinition};

// Moved to `sovereign_core::embed_fn` (2026-08-06) so the shipped
// `sovereign-cli` can build `svrn code index`'s embed function without taking
// a dependency on this crate — which would drag LanceDB, Arrow, Parquet and
// pdfium into the end-user binary. Re-exported at the historical path so all
// seven existing call sites are unaffected.
pub use sovereign_core::embed_fn::inference_to_embed_fn;

/// Allow-list of `corpus_id`s eligible for the per-article dedup pass,
/// read from `SOVEREIGN_RERANK_DEDUP_CORPORA`.
///
/// An explicitly empty string means "no filter" (apply to all corpora,
/// the original cross-corpus ablation behaviour); an unset variable
/// means the shipped default of `{sep}`. Those are different answers,
/// which is why this cannot collapse to `unwrap_or_default`.
pub fn rerank_dedup_filter_from_env() -> Option<std::collections::HashSet<String>> {
    match std::env::var("SOVEREIGN_RERANK_DEDUP_CORPORA") {
        Ok(s) if s.is_empty() => None,
        Ok(s) => Some(
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
        ),
        Err(_) => Some(["sep".to_string()].into_iter().collect()),
    }
}

/// Which chunk wins per article during dedup: `fused` (default, RRF /
/// blended-score order) or `vector` (cosine distance to the query).
pub fn rerank_dedup_picker_from_env() -> corpus_engine::DedupPicker {
    match std::env::var("SOVEREIGN_RERANK_DEDUP_PICKER")
        .as_deref()
        .unwrap_or("fused")
    {
        "vector" | "vector_distance" => corpus_engine::DedupPicker::VectorDistance,
        _ => corpus_engine::DedupPicker::FusedScore,
    }
}

/// Resolve the full `SOVEREIGN_RERANK_*` environment into a
/// [`corpus_engine::RerankConfig`].
///
/// **One decider for the rerank knobs.** The CLI, the daemon and the
/// desktop all resolve them here, so the three surfaces cannot answer
/// "what is `candidates_k`?" differently (ARCH_PRINCIPLES §10.6). It
/// traces the resolved config at `info` so an operator can see what a
/// running process actually decided without attaching a debugger —
/// previously only the CLI printed this, and only to stderr.
///
/// `SOVEREIGN_RERANK_GATE_ONLY=1` installs the rerank *function* for
/// consumers like the PPR admission gate (`SOVEREIGN_PPR_EXPAND`)
/// while leaving `enabled = false`, so the leaf search stays
/// byte-identical to baseline.
/// THE reader of `SOVEREIGN_DISABLE_WIKI_GRAPH` (TOPOLOGY §10 phase 10,
/// ARCH §10.6).
///
/// The memory-pressure escape hatch. The graph is a 7M-edge sqlite mmap; on a
/// host already running the daemon, loading it twice has tipped past available
/// RAM in practice.
///
/// The shared recipe and the desktop each carried this three-line parse, and
/// the desktop's own comment said so — "probe logic mirrors bootstrap.rs
/// `load_wikipedia_graph`; dedup to a shared crate is a follow-up". This is
/// the follow-up. Both hosts reach `sovereign-tools`, which is why the
/// predicate lives here rather than in either of them.
pub fn wiki_graph_disabled() -> bool {
    std::env::var("SOVEREIGN_DISABLE_WIKI_GRAPH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// THE reader of `SOVEREIGN_RERANK_CANDIDATES_K` (TOPOLOGY §10 phase 10,
/// ARCH §10.6).
///
/// `None` means unset or unparseable, which are the same instruction to the
/// caller — keep the default — and are distinguished in the trace rather than
/// in the type. The shared recipe had its own identical parse in its
/// dedup-only ablation branch, so an operator tuning one number could get two
/// answers depending on which host built the config.
pub fn rerank_candidates_k_from_env() -> Option<usize> {
    let raw = std::env::var("SOVEREIGN_RERANK_CANDIDATES_K").ok()?;
    match raw.parse::<usize>() {
        Ok(n) => Some(n),
        Err(_) => {
            tracing::warn!(
                value = %raw,
                "SOVEREIGN_RERANK_CANDIDATES_K is not a number — keeping the default"
            );
            None
        }
    }
}

pub fn rerank_config_from_env() -> corpus_engine::RerankConfig {
    let mut cfg = corpus_engine::RerankConfig::default();
    let gate_only = std::env::var("SOVEREIGN_RERANK_GATE_ONLY")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    cfg.enabled = !gate_only;
    if let Some(n) = rerank_candidates_k_from_env() {
        cfg.candidates_k = n;
    }
    if let Ok(s) = std::env::var("SOVEREIGN_RERANK_MIN_SCORE") {
        if let Ok(f) = s.parse::<f32>() {
            cfg.min_score = Some(f);
        }
    }
    if let Ok(s) = std::env::var("SOVEREIGN_RERANK_ALPHA") {
        if let Ok(f) = s.parse::<f32>() {
            cfg.alpha = f;
        }
    }
    if let Ok(s) = std::env::var("SOVEREIGN_RERANK_PER_ARTICLE") {
        cfg.per_article = s == "1" || s.eq_ignore_ascii_case("true");
    }
    if let Ok(s) = std::env::var("SOVEREIGN_RERANK_ATLAS_WEIGHT") {
        if let Ok(f) = s.parse::<f32>() {
            cfg.atlas_weight = f;
        }
    }
    cfg.dedup_corpus_filter = rerank_dedup_filter_from_env();
    cfg.dedup_picker = rerank_dedup_picker_from_env();
    tracing::info!(
        enabled = cfg.enabled,
        gate_only,
        candidates_k = cfg.candidates_k,
        alpha = cfg.alpha,
        per_article = cfg.per_article,
        atlas_weight = cfg.atlas_weight,
        min_score = ?cfg.min_score,
        dedup_picker = ?cfg.dedup_picker,
        dedup_corpora = ?cfg.dedup_corpus_filter.as_ref().map(|s| {
            let mut v: Vec<&String> = s.iter().collect();
            v.sort();
            v
        }),
        "rerank config resolved from environment"
    );
    cfg
}

/// Create a corpus-engine `RerankFn` from Sovereign's `InferenceProvider`.
/// Uses the provider's `rerank_batch` method which, on `EmbeddedLlamaCpp`,
/// dispatches to the installed `RerankSlot` (cross-encoder reranker).
///
/// Providers without a reranker return `Error::NotImplemented` from
/// `rerank_batch`; the wrapper converts that into
/// `corpus_engine::Error::Rerank`. The search path
/// (`CorpusIndex::search_with_rerank`) catches that and falls back
/// to the un-reranked fusion result — so installing this wrapper is
/// always safe.
pub fn inference_to_rerank_fn(inference: Arc<dyn InferenceProvider>) -> corpus_engine::RerankFn {
    Arc::new(move |query: &str, docs: Vec<String>| {
        let inf = Arc::clone(&inference);
        let query = query.to_string();
        Box::pin(async move {
            inf.rerank_batch(&query, &docs)
                .await
                .map_err(|e| corpus_engine::Error::Rerank(e.to_string()))
        })
    })
}

/// Create a corpus-engine `BatchEmbedFn` from Sovereign's `InferenceProvider`.
/// Uses the provider's `embed_batch` method which, on `EmbeddedLlamaCpp`,
/// packs multiple sequences into a single llama.cpp decode call for
/// significantly higher throughput.
pub fn inference_to_batch_embed_fn(
    inference: Arc<dyn InferenceProvider>,
) -> corpus_engine::BatchEmbedFn {
    Arc::new(move |texts: &[String]| {
        let inf = Arc::clone(&inference);
        let texts = texts.to_vec();
        Box::pin(async move {
            inf.embed_batch(&texts)
                .await
                .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
        })
    })
}

/// Create a corpus-engine `InferenceFn` from Sovereign's `InferenceProvider`.
/// Used by the optional enrichment pipeline to run claim and relationship
/// extraction prompts.
///
/// Uses `Speed::Fast` (the smaller always-loaded model) with thinking disabled
/// and a capped token budget. Structured JSON extraction doesn't benefit from
/// the 27B primary model or from chain-of-thought reasoning, and running the
/// primary model at ~1 min/chunk makes enrichment impractical on large corpora.
pub fn inference_to_inference_fn(
    inference: Arc<dyn InferenceProvider>,
) -> corpus_engine::InferenceFn {
    use sovereign_core::slot_policy::Workload;
    use sovereign_core::types::CompletionRequest;
    Arc::new(move |prompt: &str, schema: Option<&serde_json::Value>| {
        let inf = Arc::clone(&inference);
        // Schema (when the caller passes one) gates llama-cpp's
        // structured-output sampler so the response is forced into a
        // grammar matching the JSON Schema. Phase 1b on business_email
        // sets this to drop the ~54% JSON-parse-failure tail observed
        // on enron-sample-multi-wide; other phases pass None and get
        // the legacy free-form path. Owned clone is required since
        // the future moves the request.
        let structured_output = schema.cloned();
        // SLOT_POLICY §3 EnrichBulk: high-volume corpus claim/relationship
        // extraction where fast-class throughput is existential (the primary
        // model at ~1 min/chunk makes enrichment impractical on large
        // corpora) and quality is bench-validated per recipe.
        let mut request = Workload::EnrichBulk
            .request(prompt)
            // POLICY-DEBT(SLOT_POLICY §4.5 EnrichBulk): 4096 > 512 forfeits
            // the batched FastShort claim; kept — dropped 2026-05-29
            // (evening): once grammar-constrained decoding lands via
            // `structured_output`, the cap stops being load-bearing for JSON
            // validity — the schema guarantees valid array close at any token
            // count. Bigger cap (8192) just gives a rambling model more rope:
            // observed 286s batches generating 10358 tokens after grammar lit
            // up, dragging mean latency to 110s/batch. 4096 caps wall clock at
            // ~80s/batch worst case while still fitting most observed valid
            // bodies; over-cap batches end with a smaller-but-valid entity
            // list (acceptable recall hit vs the throughput win).
            .with_output_budget(4096);
        request.temperature = Some(0.1); // low temperature for consistent JSON output
        request.structured_output = structured_output;
        // POLICY-DEBT(SLOT_POLICY §3 EnrichBulk): Some(0) preserved for P1
        // neutrality (bundle is None); P5 confirms.
        request.think_budget = Some(0); // suppress thinking — hurts JSON, wastes tokens
        Box::pin(async move {
            let resp = inf
                .complete(&request)
                .await
                .map_err(|e| corpus_engine::Error::Embed(format!("inference: {e}")))?;
            Ok(resp.text)
        })
    })
}

/// A parser that converts a raw corpus source (file or directory) into
/// a streaming iterator of DocumentChunks.
///
/// Implementations must stream results — never load an entire corpus into
/// memory. The iterator yields one `Result<DocumentChunk>` at a time so
/// individual parse failures can be skipped without aborting the corpus.
pub trait CorpusParser: Send + Sync {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>>;
}

// ─── Shared Utilities ─────────────────────────────────────────

use sovereign_core::time::unix_now as now;

/// Build a DocumentChunk for a corpus entry.
fn make_chunk(corpus_id: &str, source: &str, content: &str, chunk_index: usize) -> DocumentChunk {
    DocumentChunk {
        id: format!("{corpus_id}:{source}:{chunk_index}"),
        source: source.to_string(),
        content: content.to_string(),
        chunk_index,
        embedding: None,
        created_at: now(),
        source_type: SourceType::Corpus {
            corpus_id: corpus_id.to_string(),
        },
        version: 0,
        deleted_at: None,
    }
}

/// Chunk text and wrap each piece as a DocumentChunk for the given corpus.
fn chunk_and_wrap(
    corpus_id: &str,
    source: &str,
    text: &str,
    start_index: &mut usize,
) -> Vec<DocumentChunk> {
    chunk_text(text)
        .into_iter()
        .map(|tc| {
            let idx = *start_index;
            *start_index += 1;
            make_chunk(corpus_id, source, &tc.content, idx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this file carried until 2026-08-20, asserted so a re-fork
    /// cannot bring it back silently.
    ///
    /// `strip_html` lived here as a hand-copy of
    /// `corpus_engine::extractors::strip_html`, and the two DRIFTED: the
    /// corpus-engine copy grew a clause that keeps the leading `/` of a
    /// closing tag as part of the tag name, and this copy never got it.
    /// Without it, `</script>` parses with an EMPTY tag name — the `/` is
    /// consumed by the same branch that ends tag-name collection — so
    /// `in_script` is never cleared and every character after the first
    /// `</script>` in the document is discarded. That is silent truncation on
    /// three live paths: `html_crawl` (crawled pages), `stackexchange`, and
    /// `sec_edgar` (filing bodies), all of which put a script in `<head>`.
    ///
    /// Watched failing against the local copy before the redirect landed:
    /// `assert!(text.contains("came for"))` returned an empty string.
    #[test]
    fn script_close_tag_does_not_truncate_the_document() {
        let html = "<html><head><script>var a = 1;</script></head>\
                    <body><p>The paragraph a reader came for.</p></body></html>";
        let text = strip_html(html);
        assert!(
            text.contains("The paragraph a reader came for."),
            "everything after </script> was dropped; got {text:?}"
        );
        assert!(!text.contains("var a"), "script body leaked; got {text:?}");
    }

    /// Same shape, `</style>`. The `/`-in-tag-name clause fixes both, and a
    /// stylesheet in `<head>` is at least as common as a script.
    #[test]
    fn style_close_tag_does_not_truncate_the_document() {
        let html = "<html><head><style>body { color: red; }</style></head>\
                    <body><p>Body text.</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Body text."), "got {text:?}");
        assert!(
            !text.contains("color: red"),
            "style body leaked; got {text:?}"
        );
    }
}
