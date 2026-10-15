// SPDX-License-Identifier: AGPL-3.0-or-later
//! A document body as RESOLVE reads it, with each word's raw span (split
//! from `passes.rs`).

use std::ops::Range;

/// The body whitespace-folded as RESOLVE reads it (`SourceDocument::body`),
/// with each word's raw span, so a statement located on raw lines is asked
/// over the folded text its question was measured on.
pub(super) struct Folded {
    pub(super) text: String,
    /// (raw start, raw end, folded start) per word.
    words: Vec<(usize, usize, usize)>,
}

impl Folded {
    pub(super) fn of(raw: &str) -> Self {
        let mut text = String::with_capacity(raw.len());
        let mut words = Vec::new();
        for word in raw.split_whitespace() {
            // A word is a subslice of `raw`: its offset is exact, never searched for.
            let start = word.as_ptr() as usize - raw.as_ptr() as usize;
            if !text.is_empty() {
                text.push(' ');
            }
            words.push((start, start + word.len(), text.len()));
            text.push_str(word);
        }
        Self { text, words }
    }

    /// The folded span of the words a raw span covers.
    pub(super) fn span(&self, raw: Range<usize>) -> Range<usize> {
        let inside: Vec<&(usize, usize, usize)> = self
            .words
            .iter()
            .filter(|(s, e, _)| *s >= raw.start && *e <= raw.end)
            .collect();
        match (inside.first(), inside.last()) {
            (Some(first), Some(last)) => first.2..last.2 + (last.1 - last.0),
            _ => 0..0,
        }
    }
}
