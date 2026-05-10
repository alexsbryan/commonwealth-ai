use super::{Chunker, TextChunk};

/// Heading-boundary chunker.
///
/// Splits text at heading boundaries (lines starting with `#`, `==`, or
/// followed by a line of `=` or `-` characters) and accumulates sections
/// until the max character limit is reached.
pub struct SemanticChunker {
    pub max_chars: usize,
}

impl Default for SemanticChunker {
    fn default() -> Self {
        Self { max_chars: 2048 }
    }
}

impl SemanticChunker {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Chunker for SemanticChunker {
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

        let sections = split_at_headings(text);
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut chunk_index = 0;

        for section in &sections {
            let section = section.trim();
            if section.is_empty() {
                continue;
            }

            // If this single section exceeds max, just emit it as its own chunk.
            if section.len() > self.max_chars {
                // Flush current buffer first.
                if !current.is_empty() {
                    let content = current.trim().to_string();
                    if !content.is_empty() {
                        chunks.push(TextChunk {
                            content,
                            index: chunk_index,
                        });
                        chunk_index += 1;
                    }
                    current.clear();
                }
                // Break oversized section into roughly max_chars pieces at
                // paragraph boundaries.
                let sub_sections = split_large_section(section, self.max_chars);
                for sub in sub_sections {
                    let content = sub.trim().to_string();
                    if !content.is_empty() {
                        chunks.push(TextChunk {
                            content,
                            index: chunk_index,
                        });
                        chunk_index += 1;
                    }
                }
                continue;
            }

            // If adding this section would exceed the limit, finalize.
            if !current.is_empty() && current.len() + section.len() + 2 > self.max_chars {
                let content = current.trim().to_string();
                if !content.is_empty() {
                    chunks.push(TextChunk {
                        content,
                        index: chunk_index,
                    });
                    chunk_index += 1;
                }
                current.clear();
            }

            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(section);
        }

        let final_content = current.trim().to_string();
        if !final_content.is_empty() {
            chunks.push(TextChunk {
                content: final_content,
                index: chunk_index,
            });
        }

        chunks
    }
}

/// Detect whether a line is a heading:
/// - Lines starting with `#` (Markdown ATX headings)
/// - Lines starting with `==` (MediaWiki section headers)
/// - Lines followed by a line of `===` or `---` (Setext headings, checked separately)
fn is_heading_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return true;
    }
    if trimmed.starts_with("==") && trimmed.ends_with("==") && trimmed.len() > 4 {
        return true;
    }
    false
}

/// Check whether a line is a setext-style underline (e.g., `===` or `---`).
fn is_setext_underline(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    trimmed.chars().all(|c| c == '=') || trimmed.chars().all(|c| c == '-')
}

/// Split text into sections at heading boundaries.
fn split_at_headings(text: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Check for setext-style headings: current line is text, next is underline.
        if i + 1 < lines.len() && is_setext_underline(lines[i + 1]) && !line.trim().is_empty() {
            // This line + next line form a heading. Start a new section.
            if !current.is_empty() {
                sections.push(current);
                current = String::new();
            }
            current.push_str(line);
            current.push('\n');
            current.push_str(lines[i + 1]);
            current.push('\n');
            i += 2;
            continue;
        }

        if is_heading_line(line) {
            if !current.is_empty() {
                sections.push(current);
                current = String::new();
            }
        }

        current.push_str(line);
        current.push('\n');
        i += 1;
    }

    if !current.is_empty() {
        sections.push(current);
    }

    sections
}

/// Break a large section into pieces at paragraph boundaries (\n\n).
fn split_large_section(text: &str, max_chars: usize) -> Vec<String> {
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let mut pieces = Vec::new();
    let mut current = String::new();

    for para in paragraphs {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if !current.is_empty() && current.len() + para.len() + 2 > max_chars {
            pieces.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_empty() {
        let chunker = SemanticChunker::new(100);
        assert!(chunker.chunk("").is_empty());
        assert!(chunker.chunk("   ").is_empty());
    }

    #[test]
    fn chunk_small_text() {
        let chunker = SemanticChunker::new(100);
        let chunks = chunker.chunk("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
    }

    #[test]
    fn chunk_splits_at_markdown_headings() {
        let chunker = SemanticChunker::new(100);
        let text = "# Introduction\n\nSome intro text here.\n\n# Methods\n\nSome methods text here.\n\n# Results\n\nSome results text here.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() >= 2, "Expected multiple chunks, got {}", chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    #[test]
    fn chunk_splits_at_mediawiki_headings() {
        let chunker = SemanticChunker::new(60);
        let text = "Lead paragraph with enough text.\n\n== History ==\n\nHistory text here is long enough.\n\n== Features ==\n\nFeatures text here is also long.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() >= 2, "Expected multiple chunks, got {}", chunks.len());
    }

    #[test]
    fn chunk_splits_at_setext_headings() {
        let chunker = SemanticChunker::new(80);
        let text = "Introduction\n============\n\nSome intro.\n\nMethods\n-------\n\nSome methods text here that is long.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() >= 2, "Expected at least 2 chunks, got {}", chunks.len());
    }

    #[test]
    fn chunk_accumulates_small_sections() {
        let chunker = SemanticChunker::new(500);
        let text = "# A\n\nShort.\n\n# B\n\nShort.\n\n# C\n\nShort.";
        let chunks = chunker.chunk(text);
        // All sections are small enough to fit in one chunk.
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn chunk_indices_sequential() {
        let chunker = SemanticChunker::new(50);
        let text = "# Section 1\n\nContent one.\n\n# Section 2\n\nContent two.\n\n# Section 3\n\nContent three.";
        let chunks = chunker.chunk(text);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }
}
