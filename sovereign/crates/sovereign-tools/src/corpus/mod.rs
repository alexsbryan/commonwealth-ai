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
pub fn rerank_config_from_env() -> corpus_engine::RerankConfig {
    let mut cfg = corpus_engine::RerankConfig::default();
    let gate_only = std::env::var("SOVEREIGN_RERANK_GATE_ONLY")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    cfg.enabled = !gate_only;
    if let Ok(s) = std::env::var("SOVEREIGN_RERANK_CANDIDATES_K") {
        if let Ok(n) = s.parse::<usize>() {
            cfg.candidates_k = n;
        }
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
        let mut request = CompletionRequest::for_workload(Workload::EnrichBulk, prompt)
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

/// Strip HTML tags and decode common entities.
pub(crate) fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_name = String::new();
    let mut collecting_tag_name = false;

    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            in_tag = true;
            collecting_tag_name = true;
            tag_name.clear();
            continue;
        }
        if in_tag {
            if collecting_tag_name {
                if ch.is_ascii_whitespace() || ch == '>' || ch == '/' {
                    collecting_tag_name = false;
                    let lower = tag_name.to_lowercase();
                    if lower == "script" {
                        in_script = true;
                    } else if lower == "/script" {
                        in_script = false;
                    } else if lower == "style" {
                        in_style = true;
                    } else if lower == "/style" {
                        in_style = false;
                    } else if lower == "br"
                        || lower == "br/"
                        || ((lower == "p" || lower == "/p" || lower == "div" || lower == "/div")
                            && !result.ends_with('\n'))
                    {
                        result.push('\n');
                    }
                } else {
                    tag_name.push(ch);
                }
            }
            if ch == '>' {
                in_tag = false;
            }
            continue;
        }
        if in_script || in_style {
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            for ec in chars.by_ref() {
                if ec == ';' {
                    break;
                }
                entity.push(ec);
                if entity.len() > 10 {
                    break;
                }
            }
            match entity.as_str() {
                "amp" => result.push('&'),
                "lt" => result.push('<'),
                "gt" => result.push('>'),
                "quot" => result.push('"'),
                "apos" => result.push('\''),
                "nbsp" => result.push(' '),
                s if s.starts_with('#') => {
                    let num_str = &s[1..];
                    let code = if let Some(hex) = num_str.strip_prefix('x') {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num_str.parse::<u32>().ok()
                    };
                    if let Some(c) = code.and_then(char::from_u32) {
                        result.push(c);
                    }
                }
                _ => {
                    result.push('&');
                    result.push_str(&entity);
                    result.push(';');
                }
            }
            continue;
        }
        result.push(ch);
    }

    // Collapse excessive whitespace.
    let mut collapsed = String::with_capacity(result.len());
    let mut prev_newline = false;
    for line in result.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_newline {
                collapsed.push('\n');
                prev_newline = true;
            }
        } else {
            collapsed.push_str(trimmed);
            collapsed.push('\n');
            prev_newline = false;
        }
    }

    collapsed.trim().to_string()
}
