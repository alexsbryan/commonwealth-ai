use super::{Chunker, TextChunk};

/// Sentence-boundary chunker.
///
/// Splits text on sentence boundaries (". ", "? ", "! ") and accumulates
/// sentences until the max character limit is reached.
pub struct SentenceChunker {
    pub max_chars: usize,
}

impl Default for SentenceChunker {
    fn default() -> Self {
        Self { max_chars: 2048 }
    }
}

impl SentenceChunker {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Chunker for SentenceChunker {
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

        let sentences = split_sentences(text);
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut chunk_index = 0;

        for sentence in &sentences {
            let sentence = sentence.trim();
            if sentence.is_empty() {
                continue;
            }

            // If adding this sentence would exceed the limit, finalize.
            if !current.is_empty() && current.len() + sentence.len() + 1 > self.max_chars {
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
                current.push(' ');
            }
            current.push_str(sentence);
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

/// Split text into sentences at ". ", "? ", "! " boundaries.
/// Each returned segment includes its trailing punctuation.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        if (ch == '.' || ch == '?' || ch == '!') && chars.peek() == Some(&' ') {
            // Include the trailing space in the current sentence.
            sentences.push(current.clone());
            current.clear();
        }
    }

    if !current.is_empty() {
        sentences.push(current);
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_empty() {
        let chunker = SentenceChunker::new(100);
        assert!(chunker.chunk("").is_empty());
        assert!(chunker.chunk("   ").is_empty());
    }

    #[test]
    fn chunk_small_text() {
        let chunker = SentenceChunker::new(100);
        let chunks = chunker.chunk("Hello world.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "Hello world.");
    }

    #[test]
    fn chunk_splits_at_sentences() {
        let chunker = SentenceChunker::new(50);
        let text = "First sentence. Second sentence. Third sentence. Fourth sentence.";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() > 1, "Expected multiple chunks, got {}", chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
            assert!(!chunk.content.is_empty());
        }
    }

    #[test]
    fn chunk_accumulates_short_sentences() {
        let chunker = SentenceChunker::new(200);
        let text = "A. B. C. D. E.";
        let chunks = chunker.chunk(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, text);
    }

    #[test]
    fn chunk_handles_question_and_exclamation() {
        let chunker = SentenceChunker::new(50);
        let text = "What is this? It is great! Another sentence. And more.";
        let chunks = chunker.chunk(text);
        assert!(!chunks.is_empty());
        let all: String = chunks.iter().map(|c| c.content.clone()).collect::<Vec<_>>().join(" ");
        assert!(all.contains("What is this?"));
        assert!(all.contains("It is great!"));
    }

    #[test]
    fn chunk_indices_sequential() {
        let chunker = SentenceChunker::new(30);
        let text = "One sentence. Two sentence. Three sentence. Four sentence.";
        let chunks = chunker.chunk(text);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }
}
