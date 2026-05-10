//! Passthrough chunker — emits the input as a single [`TextChunk`].
//!
//! Used by extractors that already produce chunk-sized output (e.g. the
//! `code` extractor, which yields one symbol per [`ExtractedDoc`]). Having
//! a no-op chunker means the ingest pipeline doesn't need a special
//! branch for "already chunked" extractors — the normal per-doc chunk
//! loop runs, it just sees one element.

use super::{Chunker, TextChunk};

pub struct PassthroughChunker;

impl Chunker for PassthroughChunker {
    fn chunk(&self, text: &str) -> Vec<TextChunk> {
        if text.is_empty() {
            return Vec::new();
        }
        vec![TextChunk {
            content: text.to_string(),
            index: 0,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yields_one_chunk() {
        let chunks = PassthroughChunker.chunk("fn foo() {}\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "fn foo() {}\n");
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(PassthroughChunker.chunk("").is_empty());
    }
}
