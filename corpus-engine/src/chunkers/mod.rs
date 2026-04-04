pub mod paragraph;
pub mod sentence;
pub mod fixed;
pub mod semantic;

/// A text chunk produced by a chunker.
#[derive(Debug, Clone)]
pub struct TextChunk {
    pub content: String,
    pub index: usize,
}

/// Trait for text chunking strategies.
pub trait Chunker: Send + Sync {
    fn chunk(&self, text: &str) -> Vec<TextChunk>;
}
