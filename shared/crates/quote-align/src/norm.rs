// SPDX-License-Identifier: AGPL-3.0-or-later
//! `norm_v0`: the one matching normaliser, with a map back to code points.
//!
//! NFC, then exactly the folds of `normalise_for_match`
//! (`svrn/crates/sovereign-core/src/quote_verification.rs:275-306`), then a
//! join of words hyphenated across line ends (CITE.md §5). Case is not
//! folded: a case-mismatched quotation is not verbatim
//! (`quote_verification.rs:272-274`), and whether it matters is the client's
//! call. Used for matching only; nothing is ever hashed after it.

use std::fmt;
use std::ops::Range;

use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::UnicodeNormalization;

/// A text after [`norm_v0`], remembering where every character came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormText {
    chars: Vec<char>,
    /// For each normalised character, the input code points `[from, to)` it
    /// was made from. A fold that writes several characters (`…` → `...`)
    /// gives each the same range; a whitespace run gives its one space the
    /// whole run.
    origin: Vec<(u32, u32)>,
}

impl NormText {
    /// The normalised characters.
    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    /// Normalised length, in characters.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Whether nothing survived normalisation.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The input code points a normalised range was made from. An empty
    /// range maps to the empty range at the input point it sits on.
    pub fn origin(&self, norm: Range<usize>) -> Range<usize> {
        if norm.start < norm.end {
            let from = self.origin[norm.start].0 as usize;
            let to = self.origin[norm.end - 1].1 as usize;
            from..to
        } else {
            let p = self.point(norm.start);
            p..p
        }
    }

    /// The input code point a normalised position sits on.
    fn point(&self, at: usize) -> usize {
        match (self.origin.get(at), self.origin.last()) {
            (Some(o), _) => o.0 as usize,
            (None, Some(last)) => last.1 as usize,
            (None, None) => 0,
        }
    }
}

impl fmt::Display for NormText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.chars.iter().try_for_each(|c| write!(f, "{c}"))
    }
}

/// Normalise `s` for matching (module docs), keeping the map back to its
/// code points.
pub fn norm_v0(s: &str) -> NormText {
    let input: Vec<char> = s.chars().collect();
    let mut out = Builder::default();
    let mut at = 0;
    while at < input.len() {
        let end = cluster_end(&input, at);
        for c in input[at..end].iter().copied().nfc() {
            out.push(c, (at as u32, end as u32));
        }
        at = end;
    }
    out.finish()
}

/// One past the last input character NFC may fold into the character at
/// `start`: every following non-starter, and any starter that composes with
/// what came before it (Hangul jamo, some Indic vowel signs). Composing each
/// such cluster alone is what lets every output character name its inputs.
fn cluster_end(input: &[char], start: usize) -> usize {
    let mut end = start + 1;
    while let Some(&c) = input.get(end) {
        if canonical_combining_class(c) != 0 {
            end += 1;
            continue;
        }
        // No primary composite has an ASCII second element.
        if c.is_ascii() {
            break;
        }
        let joint = input[start..=end].iter().copied().nfc().count();
        let apart =
            input[start..end].iter().copied().nfc().count() + std::iter::once(c).nfc().count();
        if joint < apart {
            end += 1;
        } else {
            break;
        }
    }
    end
}

/// What `normalise_for_match` does to one character.
enum Fold {
    Keep(char),
    Drop,
    Ellipsis,
}

fn fold(c: char) -> Fold {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{02BC}' => Fold::Keep('\''),
        '\u{201C}' | '\u{201D}' => Fold::Keep('"'),
        '\u{2013}' | '\u{2014}' => Fold::Keep('-'),
        '*' | '`' | '_' => Fold::Drop,
        '\u{2026}' => Fold::Ellipsis,
        c => Fold::Keep(c),
    }
}

/// Hyphens a line end may split a word at. A dash folded to `-` is not one:
/// "word—\nword" stays two words.
fn is_line_end_hyphen(raw: char) -> bool {
    matches!(raw, '-' | '\u{2010}' | '\u{00AD}')
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{0085}')
}

#[derive(Default)]
struct Builder {
    chars: Vec<char>,
    origin: Vec<(u32, u32)>,
    /// The character each output character was folded from.
    raw: Vec<char>,
    /// A whitespace run not yet written: its input range, and whether it
    /// holds a line break.
    pending_space: Option<(u32, u32, bool)>,
}

impl Builder {
    fn push(&mut self, c: char, from: (u32, u32)) {
        if c.is_whitespace() {
            let run = self.pending_space.get_or_insert((from.0, from.1, false));
            run.0 = run.0.min(from.0);
            run.1 = run.1.max(from.1);
            run.2 |= is_line_break(c);
            return;
        }
        match fold(c) {
            // Dropped markers neither write nor end a whitespace run, so
            // "a _ b" folds to "a b", as `normalise_for_match` does.
            Fold::Drop => {}
            Fold::Keep(k) => {
                self.flush_space(k);
                self.emit(k, c, from);
            }
            Fold::Ellipsis => {
                self.flush_space('.');
                for _ in 0..3 {
                    self.emit('.', c, from);
                }
            }
        }
    }

    /// Write the pending whitespace run as one space before `next`, unless
    /// it leads the text (trimmed) or it is the line end of a hyphenated
    /// word, which joins: "co-\noperation" → "cooperation".
    fn flush_space(&mut self, next: char) {
        let Some((from, to, line_break)) = self.pending_space.take() else {
            return;
        };
        if self.chars.is_empty() {
            return;
        }
        if line_break && next.is_lowercase() && self.ends_with_split_word() {
            self.chars.pop();
            self.origin.pop();
            self.raw.pop();
            return;
        }
        self.emit(' ', ' ', (from, to));
    }

    fn ends_with_split_word(&self) -> bool {
        let n = self.chars.len();
        n >= 2 && is_line_end_hyphen(self.raw[n - 1]) && self.chars[n - 2].is_alphabetic()
    }

    fn emit(&mut self, c: char, raw: char, from: (u32, u32)) {
        self.chars.push(c);
        self.origin.push(from);
        self.raw.push(raw);
    }

    /// A trailing whitespace run is trimmed.
    fn finish(self) -> NormText {
        NormText {
            chars: self.chars,
            origin: self.origin,
        }
    }
}

#[cfg(test)]
#[path = "norm_tests.rs"]
mod tests;
