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
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::error::Result;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{DocumentChunk, SourceType};

use crate::rag::chunk::chunk_text;

pub use manager::{CorpusInstallPhase, CorpusManager, CorpusProgress, ProgressCallback};
pub use registry::{CorpusDefinition, CorpusRegistry, TierDefinition};

/// Create a corpus-engine `EmbedFn` from Sovereign's `InferenceProvider`.
pub fn inference_to_embed_fn(inference: Arc<dyn InferenceProvider>) -> corpus_engine::EmbedFn {
    Arc::new(move |text: &str| {
        let inf = Arc::clone(&inference);
        let text = text.to_string();
        Box::pin(async move {
            inf.embed(&text)
                .await
                .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
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
    use sovereign_core::types::{CompletionRequest, Speed};
    Arc::new(move |prompt: &str| {
        let inf = Arc::clone(&inference);
        let request = CompletionRequest {
            prompt: prompt.to_string(),
            system_message: None,
            preferred_speed: Speed::Fast,  // fast model — structured extraction doesn't need 27B
            max_tokens: Some(2048),        // skeleton extraction returns structured JSON for 4 passages
            temperature: Some(0.1),        // low temperature for consistent JSON output
            think_budget: Some(0),         // suppress thinking — hurts JSON, wastes tokens
            structured_output: None,
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
        };
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
    fn parse(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>>;
}

// ─── Shared Utilities ─────────────────────────────────────────

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Build a DocumentChunk for a corpus entry.
fn make_chunk(
    corpus_id: &str,
    source: &str,
    content: &str,
    chunk_index: usize,
) -> DocumentChunk {
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
                    } else if lower == "br" || lower == "br/" {
                        result.push('\n');
                    } else if lower == "p" || lower == "/p" || lower == "div" || lower == "/div" {
                        if !result.ends_with('\n') {
                            result.push('\n');
                        }
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
