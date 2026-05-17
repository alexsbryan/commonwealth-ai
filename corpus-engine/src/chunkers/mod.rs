pub mod paragraph;
pub mod sentence;
pub mod fixed;
pub mod semantic;
pub mod passthrough;
pub mod portal_event_bullet;
pub mod sectioned;
pub mod threaded_turns;

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

/// Return the largest byte index ≤ `pos` that falls on a UTF-8 char boundary
/// in `text`. Equivalent to the nightly `str::floor_char_boundary`.
///
/// Use this any time a byte index is computed arithmetically (e.g. via
/// `saturating_sub`) before being used to slice a `&str`, so that
/// multi-byte characters (curly quotes, em-dashes, …) don't cause panics.
#[inline]
pub(crate) fn floor_char_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    if text.is_char_boundary(pos) {
        return pos;
    }
    (0..pos).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::floor_char_boundary;

    #[test]
    fn floor_char_boundary_ascii() {
        let s = "hello world";
        assert_eq!(floor_char_boundary(s, 5), 5);
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, s.len()), s.len());
    }

    #[test]
    fn floor_char_boundary_multibyte() {
        // "\u{201C}" is LEFT DOUBLE QUOTATION MARK = 3 bytes: e2 80 9c
        let s = "foo\u{201C}bar";
        // byte 3 = start of the 3-byte char → valid
        assert_eq!(floor_char_boundary(s, 3), 3);
        // byte 4 = second byte of the 3-byte char → walk back to 3
        assert_eq!(floor_char_boundary(s, 4), 3);
        // byte 5 = third byte of the 3-byte char → walk back to 3
        assert_eq!(floor_char_boundary(s, 5), 3);
        // byte 6 = start of 'b' → valid
        assert_eq!(floor_char_boundary(s, 6), 6);
    }

    #[test]
    fn floor_char_boundary_beyond_len() {
        let s = "hi";
        assert_eq!(floor_char_boundary(s, 999), s.len());
    }
}
