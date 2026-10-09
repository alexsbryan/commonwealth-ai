// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod alignment_workspace;
pub mod anthropic_export;
pub mod chatgpt_export;
pub mod column_aware;
pub mod csv;
pub mod custom_file;
pub mod described_asset;
pub mod docx;
pub mod email_rfc5322;
pub mod gutenberg_catalog;
pub mod html;
pub mod html_sections;
pub mod json;
pub mod json_api;
pub mod parquet;
pub mod plaintext;
pub mod tabular_atoms;
pub mod wikipedia_api_article;
pub mod wikipedia_catalog;
pub mod wikipedia_jsonl;
pub mod wikipedia_structured;
pub mod wikipedia_types;
pub mod xlsx;
pub mod xml;
pub mod xml_sections;

#[cfg(feature = "markdown")]
pub mod markdown;
#[cfg(feature = "markdown")]
pub mod markdown_types;

#[cfg(feature = "treesitter")]
pub mod code;

use crate::error::Result;

/// A raw document extracted from a source, before chunking.
#[derive(Debug, Clone)]
pub struct ExtractedDoc {
    pub title: Option<String>,
    pub content: String,
    pub url: Option<String>,
    pub source_id: String,
    pub metadata: Option<serde_json::Value>,
    /// The source file this document came from (filename only, e.g.
    /// `"train-00021-of-00041.parquet"`). Set by multi-shard extractors
    /// (HuggingFace parquet, JSONL splits) to enable per-file progress
    /// tracking and collaborative ingestion.  `None` for single-file sources.
    pub source_file: Option<String>,
    /// Optional override for the text used to compute the chunk's vector
    /// embedding. When `Some`, the ingest pipeline embeds this string
    /// instead of `content` (FTS still indexes the full `content`). Only
    /// honored when the configured chunker yields exactly one chunk for
    /// the document — i.e. paired with `passthrough` chunking. Used by
    /// the StackExchange `question_with_answers` mode to embed a
    /// purpose-built breadth summary (question title + first sentence of
    /// each answer) instead of the multi-thousand-token full thread,
    /// which would silently truncate to the embedding model's context
    /// window. `None` for the common case.
    pub embed_text: Option<String>,
    /// Where the bytes came from, as the stored text's record says it:
    /// `File(path)` for one file the extractor read whole, `Hashed` when a
    /// front door states the hash and its own extractor, `Record` for one
    /// record among many in a file. Required, so every extractor decides.
    pub source: DocSource,
}

pub use corpus_index::index::DocSource;

/// Trait for extracting documents from source data.
pub trait Extractor: Send + Sync {
    /// Parse the source and return an iterator of extracted documents.
    /// The iterator must be `Send` so the engine can hold it across
    /// `.await` points inside `tokio::spawn`-ed ingest tasks.
    fn extract(
        &self,
        source_path: &std::path::Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>>;
}

// ─── Shared Utilities ─────────────────────────────────────────

/// Convert a title or label into a URL-safe slug.
pub(crate) fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Collapse newlines and truncate `body` to at most `max` characters,
/// appending an ellipsis when truncated. Used as the fallback document
/// title for untitled conversations — shared by the chat-export
/// extractors ([`anthropic_export`], [`chatgpt_export`]) so both render
/// an identical legible title in retrieval surfaces. Character-aware
/// (not byte-aware) so multibyte titles never split mid-codepoint.
pub(crate) fn short_summary(body: &str, max: usize) -> String {
    let cleaned: String = body
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// Reconstruct text from an OpenAlex inverted-index JSON value.
///
/// OpenAlex encodes abstracts as `{ "word": [pos1, pos2], ... }` where
/// positions indicate word order. This function sorts by position and
/// joins the words with spaces to recover the original text.
///
/// Shared between the JSONL extractor (which reads the field from JSON)
/// and the Parquet extractor (which reads it as a string column and
/// parses it).
pub(crate) fn reconstruct_abstract(inverted_index: &serde_json::Value) -> Option<String> {
    let obj = inverted_index.as_object()?;
    let mut words: Vec<(usize, &str)> = Vec::new();
    for (word, positions) in obj {
        if let Some(arr) = positions.as_array() {
            for pos in arr {
                if let Some(idx) = pos.as_u64() {
                    words.push((idx as usize, word.as_str()));
                }
            }
        }
    }
    if words.is_empty() {
        return None;
    }
    words.sort_by_key(|(idx, _)| *idx);
    let text: String = words.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
    Some(text)
}

// The one implementation lives in the leaf svrn links too.
pub use corpus_engine_sections::strip::strip_html;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(slug("Hello World"), "hello-world");
        assert_eq!(slug("Test 123!"), "test-123");
        assert_eq!(slug("a--b"), "a-b");
    }
}
