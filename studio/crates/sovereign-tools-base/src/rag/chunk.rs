// SPDX-License-Identifier: AGPL-3.0-or-later
use async_trait::async_trait;

use sovereign_contracts::error::{Error, Result};
use sovereign_contracts::traits::Tool;
use sovereign_contracts::types::*;

/// Chunking strategy: split on paragraph boundaries, respecting a max token estimate.
///
/// - Max chunk size: ~175 tokens (estimated as ~4 chars per token)
/// - Overlap: 30 tokens (~120 chars) from the end of the previous chunk
/// - Splits on double-newlines first, then single newlines, then sentence boundaries,
///   then hard-cuts at word boundaries as a last resort.
/// - Designed to handle documents of any size (100k+ words).
///
/// **Tuning rationale (2026-05-21, book-report v1.1).** Was 2048 chars
/// (~512 tokens). Conrad's paragraphs hold multiple distinct ideas in
/// a single 2 KB chunk — the embed model represented the chunk's
/// dominant topic, and load-bearing details (the sentence "stitched
/// carefully on the under side of the lapel" sitting inside a chunk
/// otherwise about Verloc-as-lodger) embedded poorly against queries
/// targeting the specific detail. Empirically the load-bearing
/// sentence was inside a 2001-char chunk; queries about it ranked
/// the chunk at position 80-200 of 316. Cutting to 700 chars / 175
/// tokens forces sentence-level granularity where Conrad's prose is
/// dense; the chunker still keeps paragraph boundaries when they
/// fit, so semantic coherence isn't sacrificed for short paragraphs.
const MAX_CHUNK_CHARS: usize = 700; // ~175 tokens at ~4 chars/token
const OVERLAP_CHARS: usize = 120; // ~30 tokens overlap

/// A text chunk with its index in the source document.
#[derive(Debug, Clone)]
pub struct TextChunk {
    pub content: String,
    pub index: usize,
}

/// Split text into semantic chunks on paragraph boundaries.
/// Handles documents of any size — from a sentence to a full book.
pub fn chunk_text(text: &str) -> Vec<TextChunk> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    if text.len() <= MAX_CHUNK_CHARS {
        return vec![TextChunk {
            content: text.to_string(),
            index: 0,
        }];
    }

    // Split into segments: prefer double-newline, fall back to single-newline.
    let segments = split_into_segments(text);

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut chunk_index = 0;

    for segment in &segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        // If this single segment exceeds max, break it down further.
        if segment.len() > MAX_CHUNK_CHARS {
            // Flush current buffer first.
            if !current.is_empty() {
                finalize_chunk(&mut chunks, &mut current, &mut chunk_index);
            }
            // Break the oversized segment at sentence/word boundaries.
            split_oversized_segment(segment, &mut chunks, &mut chunk_index);
            continue;
        }

        // If adding this segment exceeds the limit, finalize current chunk.
        if !current.is_empty() && current.len() + segment.len() + 2 > MAX_CHUNK_CHARS {
            finalize_chunk(&mut chunks, &mut current, &mut chunk_index);
        }

        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(segment);
    }

    // Final chunk.
    let final_content = current.trim().to_string();
    if !final_content.is_empty() {
        chunks.push(TextChunk {
            content: final_content,
            index: chunk_index,
        });
    }

    chunks
}

/// Split text into segments, preferring double-newlines, falling back to single.
fn split_into_segments(text: &str) -> Vec<&str> {
    let double_split: Vec<&str> = text.split("\n\n").collect();

    // If double-newline splitting produced reasonable segments, use it.
    if double_split.len() > 1 {
        return double_split;
    }

    // Fall back to single-newline splitting.
    let single_split: Vec<&str> = text.split('\n').collect();
    if single_split.len() > 1 {
        return single_split;
    }

    // No newlines at all — return the whole text as one segment
    // (will be handled by split_oversized_segment).
    vec![text]
}

/// Break an oversized segment into chunks at sentence or word boundaries.
fn split_oversized_segment(text: &str, chunks: &mut Vec<TextChunk>, chunk_index: &mut usize) {
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= MAX_CHUNK_CHARS {
            chunks.push(TextChunk {
                content: remaining.trim().to_string(),
                index: *chunk_index,
            });
            *chunk_index += 1;
            break;
        }

        // Find the best split point within MAX_CHUNK_CHARS.
        let split_at = find_split_point(remaining, MAX_CHUNK_CHARS);

        let (chunk_text, _) = remaining.split_at(split_at);
        chunks.push(TextChunk {
            content: chunk_text.trim().to_string(),
            index: *chunk_index,
        });
        *chunk_index += 1;

        // Apply overlap: start the next chunk a bit before where we cut.
        let mut overlap_start = split_at.saturating_sub(OVERLAP_CHARS);
        while overlap_start > 0 && !remaining.is_char_boundary(overlap_start) {
            overlap_start -= 1;
        }
        let overlap_start = remaining[overlap_start..split_at]
            .rfind(' ')
            .map(|i| overlap_start + i + 1)
            .unwrap_or(overlap_start);

        remaining = remaining[overlap_start..].trim_start();
    }
}

/// Find the best character position to split at, up to max_len.
/// Prefers: sentence end (. ! ?) > comma/semicolon > word boundary.
fn find_split_point(text: &str, max_len: usize) -> usize {
    // Snap max_len to a char boundary to avoid panicking on multi-byte chars.
    let mut safe_max = max_len.min(text.len());
    while safe_max > 0 && !text.is_char_boundary(safe_max) {
        safe_max -= 1;
    }
    let search_region = &text[..safe_max];

    // Try sentence boundary (last '. ' or '! ' or '? ' in the region).
    if let Some(pos) = search_region.rfind(". ") {
        if pos > max_len / 2 {
            return pos + 2;
        }
    }
    if let Some(pos) = search_region.rfind("? ") {
        if pos > max_len / 2 {
            return pos + 2;
        }
    }
    if let Some(pos) = search_region.rfind("! ") {
        if pos > max_len / 2 {
            return pos + 2;
        }
    }

    // Try clause boundary (last ', ' or '; ').
    if let Some(pos) = search_region.rfind(", ") {
        if pos > max_len / 2 {
            return pos + 2;
        }
    }

    // Fall back to word boundary.
    if let Some(pos) = search_region.rfind(' ') {
        return pos + 1;
    }

    // Absolute last resort: hard cut.
    max_len
}

/// Finalize the current buffer into a chunk, applying overlap to start the next buffer.
fn finalize_chunk(chunks: &mut Vec<TextChunk>, current: &mut String, chunk_index: &mut usize) {
    let content = current.trim().to_string();
    if content.is_empty() {
        return;
    }

    chunks.push(TextChunk {
        content: content.clone(),
        index: *chunk_index,
    });
    *chunk_index += 1;

    // Start next chunk with overlap from the end of this one.
    // Snap to a char boundary to avoid panicking on multi-byte characters
    // (common in PDF-extracted text with ligatures like fi, fl).
    let mut overlap_start = content.len().saturating_sub(OVERLAP_CHARS);
    while overlap_start > 0 && !content.is_char_boundary(overlap_start) {
        overlap_start -= 1;
    }
    let overlap_start = content[overlap_start..]
        .find(' ')
        .map(|i| overlap_start + i + 1)
        .unwrap_or(overlap_start);
    *current = content[overlap_start..].to_string();
}

/// The corpus chunker as a workflow `Step` — the exact `chunk_text` the real
/// ingest uses, exposed as a `1→N` tool so a workflow can fan a downstream step
/// (`embed:`) over its output. Reads a file `path` (or chunks inline `text`)
/// and emits a JSON-array *collection* of `{text, index}` objects.
///
/// `Read`-effect + idempotent: pure over its input, so the workflow cache skips
/// it on an unchanged file.
pub struct ChunkTool;

#[async_trait]
impl Tool for ChunkTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "chunk".to_string(),
            name: "chunk".to_string(),
            description: "Split a document (file `path`, or inline `text`) into the corpus \
                          chunker's chunks — a collection of {text, index}."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read and chunk" },
                    "text": { "type": "string", "description": "Inline text to chunk (used if no path)" }
                }
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "index": { "type": "integer" }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        // Prefer inline `text`; otherwise read the file at `path`.
        let text = match params.get("text").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => {
                let path = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Execution("chunk: need a `path` or `text`".into()))?;
                std::fs::read_to_string(path)
                    .map_err(|e| Error::Execution(format!("chunk: read {path}: {e}")))?
            }
        };
        let chunks: Vec<serde_json::Value> = chunk_text(&text)
            .into_iter()
            .map(|c| serde_json::json!({ "text": c.content, "index": c.index }))
            .collect();
        Ok(StepOutput::Json(serde_json::Value::Array(chunks)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    #[tokio::test]
    async fn chunk_tool_reads_a_file_and_emits_a_collection() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.txt");
        std::fs::write(&p, "alpha paragraph\n\nbeta paragraph").unwrap();

        let out = ChunkTool
            .execute(
                &serde_json::json!({ "path": p.to_string_lossy() }),
                &tool_ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(serde_json::Value::Array(items)) => {
                assert!(!items.is_empty());
                assert!(items[0].get("text").and_then(|v| v.as_str()).is_some());
                assert!(items[0].get("index").and_then(|v| v.as_u64()).is_some());
            }
            other => panic!("expected a JSON array collection, got {other:?}"),
        }

        // The inline `text` branch works too (no file read).
        let out2 = ChunkTool
            .execute(
                &serde_json::json!({ "text": "just one chunk" }),
                &tool_ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(
            out2,
            StepOutput::Json(serde_json::Value::Array(_))
        ));

        // Neither `path` nor `text` is a loud error.
        assert!(ChunkTool
            .execute(&serde_json::json!({}), &tool_ctx())
            .await
            .is_err());
    }

    #[test]
    fn chunk_empty() {
        assert!(chunk_text("").is_empty());
        assert!(chunk_text("   ").is_empty());
    }

    #[test]
    fn chunk_small_text() {
        let chunks = chunk_text("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn chunk_preserves_paragraphs() {
        let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = chunk_text(text);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("First paragraph."));
        assert!(chunks[0].content.contains("Third paragraph."));
    }

    #[test]
    fn chunk_splits_large_text() {
        let paragraph =
            "This is a test paragraph with enough words to take up some space. ".repeat(10);
        let text = (0..20)
            .map(|i| format!("Section {i}. {paragraph}"))
            .collect::<Vec<_>>()
            .join("\n\n");

        let chunks = chunk_text(&text);
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}",
            chunks.len()
        );

        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }

        // No chunk should vastly exceed the max (some slack for overlap).
        for chunk in &chunks {
            assert!(
                chunk.content.len() <= MAX_CHUNK_CHARS + OVERLAP_CHARS + 200,
                "Chunk {} too large: {} chars",
                chunk.index,
                chunk.content.len()
            );
        }
    }

    #[test]
    fn chunk_overlap_exists() {
        let paragraph = "Word ".repeat(200); // ~1000 chars
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}");

        let chunks = chunk_text(&text);
        if chunks.len() >= 2 {
            let end_of_first = &chunks[0].content[chunks[0].content.len().saturating_sub(50)..];
            assert!(
                chunks[1].content.contains(end_of_first.trim()),
                "Expected overlap between chunks"
            );
        }
    }

    #[test]
    fn chunk_handles_single_newlines() {
        // Text with only single newlines (no double-newline paragraphs).
        let lines: Vec<String> = (0..100)
            .map(|i| format!("Line {i} with some content that makes it a reasonable length."))
            .collect();
        let text = lines.join("\n");

        let chunks = chunk_text(&text);
        assert!(chunks.len() > 1, "Should split single-newline text");
        for chunk in &chunks {
            assert!(!chunk.content.is_empty());
        }
    }

    #[test]
    fn chunk_handles_no_newlines() {
        // One giant paragraph with no newlines at all.
        let text = "This is a sentence. ".repeat(500); // ~10000 chars

        let chunks = chunk_text(&text);
        assert!(
            chunks.len() > 1,
            "Should split text without newlines, got {} chunks",
            chunks.len()
        );

        // Splits should happen at sentence boundaries.
        for chunk in &chunks {
            assert!(
                chunk.content.len() <= MAX_CHUNK_CHARS + OVERLAP_CHARS + 200,
                "Chunk {} too large: {} chars",
                chunk.index,
                chunk.content.len()
            );
        }
    }

    #[test]
    fn chunk_handles_book_sized_document() {
        // Simulate a 100k-word document (~600k chars).
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let paragraph = sentence.repeat(50); // ~2250 chars per paragraph
        let text = (0..250)
            .map(|_| paragraph.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        assert!(text.len() > 500_000, "Test text should be >500k chars");

        let chunks = chunk_text(&text);
        assert!(
            chunks.len() > 100,
            "100k-word doc should produce many chunks, got {}",
            chunks.len()
        );

        // All chunks should be reasonably sized.
        for chunk in &chunks {
            assert!(
                chunk.content.len() <= MAX_CHUNK_CHARS + OVERLAP_CHARS + 200,
                "Chunk {} too large: {} chars",
                chunk.index,
                chunk.content.len()
            );
            assert!(!chunk.content.is_empty());
        }

        // Indices should be sequential.
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn chunk_oversized_single_paragraph() {
        // One paragraph that's 10k chars with no newlines.
        let text = "word ".repeat(2000); // ~10000 chars, no newlines

        let chunks = chunk_text(&text);
        assert!(
            chunks.len() > 1,
            "Should split oversized paragraph, got {} chunks",
            chunks.len()
        );
    }
}
