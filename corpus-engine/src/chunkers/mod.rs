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

/// One chunk previously committed to the index, with the
/// content-hash that the index recorded for it. Used by
/// [`chunk_delta`] to compute what's changed between an old
/// version of a document and its new content.
#[derive(Debug, Clone)]
pub struct CommittedChunk {
    pub id: u64,
    pub content_hash: String,
}

/// Difference between an old set of committed chunks and the
/// re-chunked new content. Move 6 P6 — pairs with
/// [`crate::engine::CorpusEngine::reindex_file`] so a single-line
/// edit to a long document re-embeds only the changed chunks
/// instead of the whole file.
#[derive(Debug, Clone, Default)]
pub struct ChunkDiff {
    /// Chunk ids (LanceDB row ids) to delete. These appear in
    /// `old_chunks` but their content_hash didn't survive into
    /// `new_chunks`.
    pub deleted: Vec<u64>,
    /// Newly-chunked content that doesn't match any old chunk's
    /// content_hash. Must be embedded + inserted.
    pub added: Vec<TextChunk>,
    /// Old chunk ids whose content_hash matched a new chunk
    /// verbatim. The caller skips re-embedding for these — the
    /// stored row stays untouched.
    pub kept_unchanged: Vec<u64>,
}

impl ChunkDiff {
    pub fn is_noop(&self) -> bool {
        self.deleted.is_empty() && self.added.is_empty()
    }
}

/// Compute the delta between an old chunk set and a freshly-chunked
/// version of the same document. Match by content_hash so chunks
/// that simply shifted position (a line inserted earlier in the
/// doc) still hash-match and are kept.
///
/// `hash_chunk` lets the caller choose the hash function (matches
/// whichever the ingest path uses — typically blake3 over the
/// content string). Returning the closure rather than hardwiring
/// blake3 keeps the chunker module dependency-free.
pub fn chunk_delta(
    old: &[CommittedChunk],
    new_chunks: Vec<TextChunk>,
    hash_chunk: impl Fn(&str) -> String,
) -> ChunkDiff {
    use std::collections::{HashMap, HashSet};

    // old_by_hash: content_hash → chunk_id. Multiple chunks may
    // share a hash (duplicate paragraphs). Map to a Vec so each
    // duplicate gets a distinct id.
    let mut old_by_hash: HashMap<String, Vec<u64>> = HashMap::new();
    for c in old {
        old_by_hash
            .entry(c.content_hash.clone())
            .or_default()
            .push(c.id);
    }

    let mut diff = ChunkDiff::default();
    let mut consumed_old_ids: HashSet<u64> = HashSet::new();

    for nc in new_chunks {
        let nhash = hash_chunk(&nc.content);
        let matched = old_by_hash.get_mut(&nhash).and_then(|v| v.pop());
        if let Some(old_id) = matched {
            diff.kept_unchanged.push(old_id);
            consumed_old_ids.insert(old_id);
        } else {
            diff.added.push(nc);
        }
    }
    for c in old {
        if !consumed_old_ids.contains(&c.id) {
            diff.deleted.push(c.id);
        }
    }
    diff
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
mod chunk_delta_tests {
    use super::*;

    fn fake_hash(s: &str) -> String {
        // Stable per-content hash for tests — content as-is works.
        format!("hash:{s}")
    }

    fn old_chunk(id: u64, content: &str) -> CommittedChunk {
        CommittedChunk {
            id,
            content_hash: fake_hash(content),
        }
    }

    fn new_chunk(idx: usize, content: &str) -> TextChunk {
        TextChunk {
            content: content.into(),
            index: idx,
        }
    }

    #[test]
    fn identical_content_yields_noop() {
        let old = vec![
            old_chunk(1, "para A"),
            old_chunk(2, "para B"),
            old_chunk(3, "para C"),
        ];
        let new = vec![
            new_chunk(0, "para A"),
            new_chunk(1, "para B"),
            new_chunk(2, "para C"),
        ];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 3);
        assert!(diff.deleted.is_empty());
        assert!(diff.added.is_empty());
        assert!(diff.is_noop());
    }

    #[test]
    fn appended_paragraph_yields_one_added_only() {
        let old = vec![old_chunk(1, "para A"), old_chunk(2, "para B")];
        let new = vec![
            new_chunk(0, "para A"),
            new_chunk(1, "para B"),
            new_chunk(2, "para C"),
        ];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 2);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].content, "para C");
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn deleted_paragraph_yields_one_deleted_only() {
        let old = vec![
            old_chunk(1, "para A"),
            old_chunk(2, "para B"),
            old_chunk(3, "para C"),
        ];
        let new = vec![new_chunk(0, "para A"), new_chunk(1, "para C")];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 2);
        assert!(diff.added.is_empty());
        assert_eq!(diff.deleted, vec![2]);
    }

    #[test]
    fn middle_edit_yields_one_delete_plus_one_add() {
        let old = vec![
            old_chunk(1, "para A"),
            old_chunk(2, "para B"),
            old_chunk(3, "para C"),
        ];
        let new = vec![
            new_chunk(0, "para A"),
            new_chunk(1, "para B-EDITED"),
            new_chunk(2, "para C"),
        ];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 2);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].content, "para B-EDITED");
        assert_eq!(diff.deleted, vec![2]);
    }

    #[test]
    fn shifted_paragraph_still_matches_by_hash() {
        // "para B" moves from position 1 → 2 (something else inserted
        // before it). chunk_delta matches by content_hash so para B
        // stays kept_unchanged.
        let old = vec![old_chunk(1, "para A"), old_chunk(2, "para B")];
        let new = vec![
            new_chunk(0, "para A"),
            new_chunk(1, "NEW para"),
            new_chunk(2, "para B"),
        ];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 2);
        assert_eq!(diff.added.len(), 1);
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn duplicate_paragraphs_match_distinct_ids() {
        // Two paragraphs with identical content map to two distinct
        // old chunk ids. After delta, both stay kept_unchanged.
        let old = vec![
            old_chunk(1, "same"),
            old_chunk(2, "same"),
        ];
        let new = vec![new_chunk(0, "same"), new_chunk(1, "same")];
        let diff = chunk_delta(&old, new, fake_hash);
        assert_eq!(diff.kept_unchanged.len(), 2);
        assert!(diff.added.is_empty());
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn full_replacement_yields_all_deleted_and_all_added() {
        let old = vec![old_chunk(1, "old A"), old_chunk(2, "old B")];
        let new = vec![new_chunk(0, "new A"), new_chunk(1, "new B")];
        let diff = chunk_delta(&old, new, fake_hash);
        assert!(diff.kept_unchanged.is_empty());
        assert_eq!(diff.added.len(), 2);
        assert_eq!(diff.deleted.len(), 2);
    }
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
