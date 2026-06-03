use super::{floor_char_boundary, Chunker, TextChunk};

/// Paragraph-boundary chunker.
///
/// - Max chunk size: configurable (default 2048 chars, ~512 tokens)
/// - Overlap: configurable (default 256 chars, ~64 tokens)
/// - Splits on double-newlines first, then single newlines, then sentence
///   boundaries, then word boundaries.
/// - Handles documents of any size.
pub struct ParagraphChunker {
    pub max_chars: usize,
    pub overlap_chars: usize,
}

impl Default for ParagraphChunker {
    fn default() -> Self {
        Self {
            max_chars: 2048,
            overlap_chars: 256,
        }
    }
}

impl ParagraphChunker {
    pub fn new(max_chars: usize, overlap_chars: usize) -> Self {
        Self {
            max_chars,
            overlap_chars,
        }
    }
}

impl Chunker for ParagraphChunker {
    fn chunk(&self, text: &str) -> Vec<TextChunk> {
        chunk_text(text, self.max_chars, self.overlap_chars)
    }
}

/// Split text into semantic chunks on paragraph boundaries.
/// Handles documents of any size -- from a sentence to a full book.
fn chunk_text(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<TextChunk> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    if text.len() <= max_chars {
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
        if segment.len() > max_chars {
            // Flush current buffer first.
            if !current.is_empty() {
                finalize_chunk(&mut chunks, &mut current, &mut chunk_index, overlap_chars);
            }
            // Break the oversized segment at sentence/word boundaries.
            split_oversized_segment(
                segment,
                &mut chunks,
                &mut chunk_index,
                max_chars,
                overlap_chars,
            );
            continue;
        }

        // If adding this segment exceeds the limit, finalize current chunk.
        if !current.is_empty() && current.len() + segment.len() + 2 > max_chars {
            finalize_chunk(&mut chunks, &mut current, &mut chunk_index, overlap_chars);
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

    // No newlines at all -- returned as one segment
    // (will be handled by split_oversized_segment).
    vec![text]
}

/// Break an oversized segment into chunks at sentence or word boundaries.
fn split_oversized_segment(
    text: &str,
    chunks: &mut Vec<TextChunk>,
    chunk_index: &mut usize,
    max_chars: usize,
    overlap_chars: usize,
) {
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_chars {
            chunks.push(TextChunk {
                content: remaining.trim().to_string(),
                index: *chunk_index,
            });
            *chunk_index += 1;
            break;
        }

        // Find the best split point within max_chars.
        let split_at = find_split_point(remaining, max_chars);

        let (chunk_text, _) = remaining.split_at(split_at);
        chunks.push(TextChunk {
            content: chunk_text.trim().to_string(),
            index: *chunk_index,
        });
        *chunk_index += 1;

        // Apply overlap: start the next chunk a bit before where we cut.
        let overlap_start = floor_char_boundary(remaining, split_at.saturating_sub(overlap_chars));
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
    // Clamp to a valid UTF-8 char boundary — byte index max_len may land
    // inside a multi-byte character (e.g. curly quotes, em-dashes).
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
fn finalize_chunk(
    chunks: &mut Vec<TextChunk>,
    current: &mut String,
    chunk_index: &mut usize,
    overlap_chars: usize,
) {
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
    let overlap_start = floor_char_boundary(&content, content.len().saturating_sub(overlap_chars));
    let overlap_start = content[overlap_start..]
        .find(' ')
        .map(|i| overlap_start + i + 1)
        .unwrap_or(overlap_start);
    *current = content[overlap_start..].to_string();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_chunk(text: &str) -> Vec<TextChunk> {
        chunk_text(text, 2048, 256)
    }

    #[test]
    fn chunk_empty() {
        assert!(default_chunk("").is_empty());
        assert!(default_chunk("   ").is_empty());
    }

    #[test]
    fn chunk_small_text() {
        let chunks = default_chunk("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn chunk_preserves_paragraphs() {
        let text = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = default_chunk(text);
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

        let chunks = default_chunk(&text);
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
                chunk.content.len() <= 2048 + 256 + 200,
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

        let chunks = default_chunk(&text);
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
        let lines: Vec<String> = (0..100)
            .map(|i| format!("Line {i} with some content that makes it a reasonable length."))
            .collect();
        let text = lines.join("\n");

        let chunks = default_chunk(&text);
        assert!(chunks.len() > 1, "Should split single-newline text");
        for chunk in &chunks {
            assert!(!chunk.content.is_empty());
        }
    }

    #[test]
    fn chunk_handles_no_newlines() {
        let text = "This is a sentence. ".repeat(500);

        let chunks = default_chunk(&text);
        assert!(
            chunks.len() > 1,
            "Should split text without newlines, got {} chunks",
            chunks.len()
        );

        for chunk in &chunks {
            assert!(
                chunk.content.len() <= 2048 + 256 + 200,
                "Chunk {} too large: {} chars",
                chunk.index,
                chunk.content.len()
            );
        }
    }

    #[test]
    fn chunk_handles_book_sized_document() {
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let paragraph = sentence.repeat(50);
        let text = (0..250)
            .map(|_| paragraph.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        assert!(text.len() > 500_000, "Test text should be >500k chars");

        let chunks = default_chunk(&text);
        assert!(
            chunks.len() > 100,
            "100k-word doc should produce many chunks, got {}",
            chunks.len()
        );

        for chunk in &chunks {
            assert!(
                chunk.content.len() <= 2048 + 256 + 200,
                "Chunk {} too large: {} chars",
                chunk.index,
                chunk.content.len()
            );
            assert!(!chunk.content.is_empty());
        }

        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn chunk_oversized_single_paragraph() {
        let text = "word ".repeat(2000);

        let chunks = default_chunk(&text);
        assert!(
            chunks.len() > 1,
            "Should split oversized paragraph, got {} chunks",
            chunks.len()
        );
    }

    #[test]
    fn custom_sizes() {
        let chunker = ParagraphChunker::new(100, 20);
        let text = "Short sentence. ".repeat(50);
        let chunks = chunker.chunk(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            // Allow some slack for overlap
            assert!(chunk.content.len() <= 100 + 20 + 50);
        }
    }

    #[test]
    fn chunker_trait_impl() {
        let chunker = ParagraphChunker::default();
        let chunks = chunker.chunk("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
    }
}
