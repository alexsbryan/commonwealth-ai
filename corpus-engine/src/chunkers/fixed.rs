use super::{Chunker, TextChunk};

/// Fixed-size chunker with overlap.
///
/// Splits text at word boundaries near the max_chars limit, with configurable
/// overlap between consecutive chunks.
pub struct FixedChunker {
    pub max_chars: usize,
    pub overlap_chars: usize,
}

impl Default for FixedChunker {
    fn default() -> Self {
        Self {
            max_chars: 2048,
            overlap_chars: 256,
        }
    }
}

impl FixedChunker {
    pub fn new(max_chars: usize, overlap_chars: usize) -> Self {
        Self {
            max_chars,
            overlap_chars,
        }
    }
}

impl Chunker for FixedChunker {
    fn chunk(&self, text: &str) -> Vec<TextChunk> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }

        if text.len() <= self.max_chars {
            return vec![TextChunk {
                content: text.to_string(),
                index: 0,
            }];
        }

        let mut chunks = Vec::new();
        let mut chunk_index = 0;
        let mut start = 0;

        while start < text.len() {
            let end = {
                let raw = (start + self.max_chars).min(text.len());
                // Clamp to a valid UTF-8 char boundary.
                if text.is_char_boundary(raw) {
                    raw
                } else {
                    (start..raw)
                        .rev()
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(start)
                }
            };

            // If we're not at the end, find a word boundary to split at.
            let split_at = if end < text.len() {
                text[start..end]
                    .rfind(' ')
                    .map(|pos| start + pos)
                    .unwrap_or(end)
            } else {
                end
            };

            let content = text[start..split_at].trim().to_string();
            if !content.is_empty() {
                chunks.push(TextChunk {
                    content,
                    index: chunk_index,
                });
                chunk_index += 1;
            }

            if split_at >= text.len() {
                break;
            }

            // Move forward, applying overlap.
            let next_start = {
                let raw = split_at.saturating_sub(self.overlap_chars);
                // Clamp to a valid UTF-8 char boundary before slicing.
                if text.is_char_boundary(raw) {
                    raw
                } else {
                    (0..raw)
                        .rev()
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(raw)
                }
            };
            // Align to a word boundary.
            let next_start = text[next_start..split_at]
                .rfind(' ')
                .map(|i| next_start + i + 1)
                .unwrap_or(next_start);

            // Ensure we always make forward progress.
            if next_start <= start {
                start = split_at + 1;
            } else {
                start = next_start;
            }
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_empty() {
        let chunker = FixedChunker::new(100, 20);
        assert!(chunker.chunk("").is_empty());
        assert!(chunker.chunk("   ").is_empty());
    }

    #[test]
    fn chunk_small_text() {
        let chunker = FixedChunker::new(100, 20);
        let chunks = chunker.chunk("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
    }

    #[test]
    fn chunk_splits_large_text() {
        let chunker = FixedChunker::new(100, 20);
        let text = "word ".repeat(200);
        let chunks = chunker.chunk(&text);
        assert!(chunks.len() > 1, "Expected multiple chunks");
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
            assert!(!chunk.content.is_empty());
        }
    }

    #[test]
    fn chunk_overlap_exists() {
        let chunker = FixedChunker::new(100, 30);
        let text = "word ".repeat(200);
        let chunks = chunker.chunk(&text);
        if chunks.len() >= 2 {
            let end_of_first = &chunks[0].content[chunks[0].content.len().saturating_sub(20)..];
            assert!(
                chunks[1].content.contains(end_of_first.trim()),
                "Expected overlap between chunks"
            );
        }
    }

    #[test]
    fn chunk_indices_sequential() {
        let chunker = FixedChunker::new(50, 10);
        let text = "word ".repeat(100);
        let chunks = chunker.chunk(&text);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn chunk_no_overlap() {
        let chunker = FixedChunker::new(100, 0);
        let text = "word ".repeat(200);
        let chunks = chunker.chunk(&text);
        assert!(chunks.len() > 1);
    }
}
