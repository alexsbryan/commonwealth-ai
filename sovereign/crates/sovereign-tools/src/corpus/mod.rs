pub mod gutenberg;
pub mod html_crawl;
pub mod manager;
pub mod openalex;
pub mod registry;
pub mod stackexchange;
pub mod wikipedia;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::error::Result;
use sovereign_core::types::{DocumentChunk, SourceType};

use crate::rag::chunk::chunk_text;

pub use manager::{CorpusInstallPhase, CorpusManager, CorpusProgress, ProgressCallback};
pub use registry::{CorpusDefinition, CorpusRegistry, TierDefinition};

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
