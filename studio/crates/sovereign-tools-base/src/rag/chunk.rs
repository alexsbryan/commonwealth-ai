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
        let overlap_start = floor_char_boundary(remaining, split_at.saturating_sub(OVERLAP_CHARS));
        let overlap_start = remaining[overlap_start..split_at]
            .rfind(' ')
            .map(|i| overlap_start + i + 1)
            .unwrap_or(overlap_start);

        remaining = remaining[overlap_start..].trim_start();
    }
}

/// Largest byte index <= `pos` that is a char boundary in `text`.
///
/// One decider for the whole file. Every byte index here is computed
/// arithmetically — `MAX_CHUNK_CHARS`, `saturating_sub(OVERLAP_CHARS)` — and
/// then used to slice, so each one has to be snapped first or a multi-byte
/// character (CJK, curly quotes, PDF ligatures) turns it into a panic.
/// Mirrors `corpus_engine::chunkers::floor_char_boundary`, which is the same
/// decision on the other half of this chunker fork.
#[inline]
fn floor_char_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    if text.is_char_boundary(pos) {
        return pos;
    }
    (0..pos)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0)
}

/// Find the best character position to split at, up to max_len.
/// Prefers: sentence end (. ! ?) > comma/semicolon > word boundary.
///
/// Every `return` is a byte index the caller will slice at, so `max_len` is
/// snapped to a char boundary ONCE, up front, and every branch below — the
/// `> max_len / 2` guards and the last-resort hard cut alike — reads the
/// snapped value. Snapping only the search region (which is what this half of
/// the fork used to do) left the hard cut returning the raw argument, and
/// space-free multi-byte prose reaches exactly that branch.
fn find_split_point(text: &str, max_len: usize) -> usize {
    let max_len = floor_char_boundary(text, max_len);
    let search_region = &text[..max_len];

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

    // Start next chunk with overlap from the end of this one. Snapped, because
    // multi-byte characters (PDF ligatures fi/fl, CJK) otherwise panic here.
    let overlap_start = floor_char_boundary(&content, content.len().saturating_sub(OVERLAP_CHARS));
    let overlap_start = content[overlap_start..]
        .find(' ')
        .map(|i| overlap_start + i + 1)
        .unwrap_or(overlap_start);
    *current = content[overlap_start..].to_string();
}

/// The paragraph chunker as a workflow `Step`, exposed as a `1→N` tool so a
/// workflow can fan a downstream step (`embed:`) over its output. Reads a file
/// `path` (or chunks inline `text`) and emits a JSON-array *collection* of
/// `{text, index}` objects.
///
/// NOT byte-identical to ingest, despite what this comment claimed until
/// 2026-08-21. It is the same ALGORITHM as
/// `corpus_engine::chunkers::paragraph` — same segment split, same split-point
/// preference order, same overlap rule — but the sizes are fixed here at
/// `MAX_CHUNK_CHARS`/`OVERLAP_CHARS` (700/120), while ingest takes them from
/// the recipe's `ChunkerConfig::Paragraph` and defaults to 2048/256
/// (`corpus-engine/src/recipe.rs`, `default_max_chunk_chars`). A workflow that
/// chunks here and a corpus that ingests there produce DIFFERENT chunk
/// boundaries; do not treat one as a preview of the other.
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
            ..Default::default()
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

    /// Space-free multi-byte text must not panic.
    ///
    /// `find_split_point`'s last-resort `max_len` used to be the RAW argument,
    /// while the search region had already been snapped DOWN to a char
    /// boundary. For CJK prose (3 bytes/char, no ASCII spaces) every earlier
    /// branch misses — `rfind(". ")`, `rfind(", ")`, `rfind(' ')` all return
    /// `None` — so the fallback fired with a byte index sitting inside a
    /// character, and the caller's `remaining.split_at(split_at)` panicked
    /// with "byte index 700 is not a char boundary".
    ///
    /// corpus-engine's copy of this same function (chunkers/paragraph.rs) has
    /// always clamped the fallback; this half of the fork had not. Any
    /// Japanese/Chinese/Thai document over MAX_CHUNK_CHARS reached it, through
    /// `tool:chunk` and `tool:section` alike.
    #[test]
    fn chunk_space_free_multibyte_text_does_not_panic() {
        // 14 chars x 3 bytes x 60 = 2520 bytes; byte 700 lands inside a char.
        let text = "\u{77e5}\u{8b58}\u{306f}\u{529b}\u{306a}\u{308a}\u{6559}\u{80b2}\u{306f}\u{672a}\u{6765}\u{3092}\u{7bc9}\u{304f}".repeat(60);
        assert!(
            !text.is_char_boundary(MAX_CHUNK_CHARS),
            "fixture must straddle a char at the cut"
        );

        let chunks = chunk_text(&text);
        assert!(
            chunks.len() > 1,
            "oversized CJK text should split, got {}",
            chunks.len()
        );
        // Nothing was dropped or corrupted: every chunk is valid UTF-8 the
        // source actually contains.
        for c in &chunks {
            assert!(!c.content.is_empty());
            assert!(
                text.contains(c.content.as_str()),
                "chunk {} is not a substring of the source",
                c.index
            );
        }
    }

    /// The last-resort branch itself, in isolation: whatever it returns must
    /// be sliceable. This is the assertion the caller depends on and the one
    /// the fork lost.
    #[test]
    fn find_split_point_fallback_returns_a_char_boundary() {
        let text = "\u{77e5}\u{8b58}\u{306f}\u{529b}\u{306a}\u{308a}\u{6559}\u{80b2}\u{306f}\u{672a}\u{6765}\u{3092}\u{7bc9}\u{304f}".repeat(60);
        let at = find_split_point(&text, MAX_CHUNK_CHARS);
        assert!(
            text.is_char_boundary(at),
            "find_split_point returned {at}, which is inside a character"
        );
        assert!(
            at <= MAX_CHUNK_CHARS,
            "returned {at}, past the requested max"
        );
    }
}
