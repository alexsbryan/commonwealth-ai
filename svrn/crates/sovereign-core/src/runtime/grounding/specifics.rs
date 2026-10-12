// SPDX-License-Identifier: AGPL-3.0-or-later
//! A text's sentences and each sentence's specifics (the numbers, dates and
//! names it asserts), found without a model: what the fast tier of
//! `POST /oicp/v1/verify` (`cmnwlth/docs/oicp-v0.6.md`) checks for presence
//! in the evidence, with [`super::value_present_in_chunks`] deciding.
//!
//! Both are shapes, not readings. A sentence ends at `.`, `!` or `?` before
//! whitespace and an uppercase letter, a digit or an opening quote, unless
//! the word before the stop is one capital (an initial, "J. S. Mill"). A
//! number is any word of two or more code points carrying a digit; a name is
//! a run of name-shaped words ([`super::judge::cap_name`]), less the words
//! that are never names ([`super::judge::non_name_word`]), where a run that
//! opens the sentence counts only once it is two words long.
//!
//! The fast tier refuses on every specific no passage carries, so a shape
//! that over-reaches is a false flag, and one that under-reaches is a miss
//! the checker tier is there for. Both exclusions were measured on X1's draft
//! over its Hume library (507 sentences, 2026-10-12): 87 of the 99 sentences
//! the tier flagged were flagged only for a one-digit number ("Book 3":
//! `value_present_in_chunks` reads words of two or more code points, so it can
//! never find one) or for the capitalised first word of the sentence
//! ("Finally", "Similarly", "Having"). The cost is a sentence that opens with
//! a one-word name ("Hume wrote…") offering that name to no check. Ranges are
//! code points.

use std::ops::Range;

use super::judge::{cap_name, non_name_word};

/// What a specific is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecificKind {
    /// A word carrying a digit: a count, a year, a date, a figure.
    Number,
    /// A run of name-shaped words: a person, a place, a work.
    Name,
}

/// One specific a sentence asserts, as it is written there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specific {
    /// Number or name.
    pub kind: SpecificKind,
    /// The specific as written, its surrounding punctuation trimmed.
    pub text: String,
    /// Code points of `text` in the sentence.
    pub range: Range<usize>,
}

/// The sentences of `text`, as code-point ranges with their surrounding
/// whitespace trimmed. Text with no stop is one sentence; whitespace alone is
/// none.
pub fn sentence_ranges(text: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '.' | '!' | '?') && ends_sentence(&chars, i) {
            let mut end = i + 1;
            while end < chars.len() && is_closer(chars[end]) {
                end += 1;
            }
            push_trimmed(&chars, start..end, &mut out);
            start = end;
            i = end;
            continue;
        }
        i += 1;
    }
    push_trimmed(&chars, start..chars.len(), &mut out);
    out
}

fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '\u{201d}' | '\u{2019}')
}

/// Whether the stop at `i` ends a sentence: closers, then whitespace, then a
/// sentence opener; never after a lone capital.
fn ends_sentence(chars: &[char], i: usize) -> bool {
    let lone_capital = i >= 1
        && chars[i - 1].is_uppercase()
        && (i == 1 || !chars[i - 2].is_alphanumeric())
        && chars[i] == '.';
    if lone_capital {
        return false;
    }
    let mut j = i + 1;
    while j < chars.len() && is_closer(chars[j]) {
        j += 1;
    }
    if j == chars.len() {
        return true;
    }
    if !chars[j].is_whitespace() {
        return false;
    }
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    j == chars.len()
        || chars[j].is_uppercase()
        || chars[j].is_ascii_digit()
        || matches!(chars[j], '"' | '\u{201c}' | '(' | '[')
}

fn push_trimmed(chars: &[char], r: Range<usize>, out: &mut Vec<Range<usize>>) {
    let (mut a, mut b) = (r.start, r.end);
    while a < b && chars[a].is_whitespace() {
        a += 1;
    }
    while b > a && chars[b - 1].is_whitespace() {
        b -= 1;
    }
    if a < b {
        out.push(a..b);
    }
}

/// The specifics `sentence` asserts, in reading order.
pub fn sentence_specifics(sentence: &str) -> Vec<Specific> {
    /// A name being built, and whether the sentence's first word opened it:
    /// a one-word run there is a capitalised first word, not a name.
    struct Run {
        range: Range<usize>,
        words: Vec<String>,
        opens_sentence: bool,
    }
    fn flush(run: &mut Option<Run>, out: &mut Vec<Specific>) {
        if let Some(r) = run.take() {
            if r.opens_sentence && r.words.len() == 1 {
                return;
            }
            out.push(Specific {
                kind: SpecificKind::Name,
                text: r.words.join(" "),
                range: r.range,
            });
        }
    }
    let mut out = Vec::new();
    let mut run: Option<Run> = None;
    let mut first = true;
    for (word, at) in words_with_offsets(sentence) {
        let Some((trimmed, lead)) = trim_word(word) else {
            flush(&mut run, &mut out);
            continue;
        };
        let opens_sentence = std::mem::replace(&mut first, false);
        let a = at + lead;
        let bare = strip_possessive(trimmed);
        if bare.chars().any(|c| c.is_ascii_digit()) {
            flush(&mut run, &mut out);
            if bare.chars().count() >= 2 {
                out.push(Specific {
                    kind: SpecificKind::Number,
                    text: bare.to_string(),
                    range: a..a + bare.chars().count(),
                });
            }
            continue;
        }
        match cap_name(bare).filter(|w| !non_name_word(w)) {
            Some(name) => {
                let b = a + name.chars().count();
                match run.as_mut() {
                    Some(r) => {
                        r.range.end = b;
                        r.words.push(name.to_string());
                    }
                    None => {
                        run = Some(Run {
                            range: a..b,
                            words: vec![name.to_string()],
                            opens_sentence,
                        })
                    }
                }
                // A separator, or a possessive, after the word ends the name:
                // "Hamilton, Madison" is two people, "Hume's Treatise" two
                // specifics.
                if bare.len() != trimmed.len() || word.ends_with([',', ';', ':', '/', '&']) {
                    flush(&mut run, &mut out);
                }
            }
            None => flush(&mut run, &mut out),
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Whitespace-separated words with the code point each starts at.
fn words_with_offsets(s: &str) -> Vec<(&str, usize)> {
    let mut out = Vec::new();
    let mut cp = 0;
    let mut start: Option<(usize, usize)> = None; // (byte, code point)
    for (byte, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some((b0, c0)) = start.take() {
                out.push((&s[b0..byte], c0));
            }
        } else if start.is_none() {
            start = Some((byte, cp));
        }
        cp += 1;
    }
    if let Some((b0, c0)) = start {
        out.push((&s[b0..], c0));
    }
    out
}

/// `word` less its surrounding punctuation, and how many code points were
/// trimmed from its front; `None` when nothing is left.
fn trim_word(word: &str) -> Option<(&str, usize)> {
    let t = word.trim_matches(|c: char| !c.is_alphanumeric());
    if t.is_empty() {
        return None;
    }
    let lead = word.find(t).map(|b| word[..b].chars().count())?;
    Some((t, lead))
}

fn strip_possessive(w: &str) -> &str {
    w.strip_suffix("'s")
        .or_else(|| w.strip_suffix("\u{2019}s"))
        .unwrap_or(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<(SpecificKind, String)> {
        sentence_specifics(s)
            .into_iter()
            .map(|x| (x.kind, x.text))
            .collect()
    }

    #[test]
    fn sentences_split_at_stops_before_an_opener_and_not_after_an_initial() {
        let t = "J. S. Mill wrote in 1859. It sold well!  \"Then\" it ended? no stop here";
        let got: Vec<String> = sentence_ranges(t)
            .into_iter()
            .map(|r| t.chars().skip(r.start).take(r.len()).collect())
            .collect();
        assert_eq!(
            got,
            vec![
                "J. S. Mill wrote in 1859.",
                "It sold well!",
                "\"Then\" it ended? no stop here",
            ]
        );
        assert!(sentence_ranges("  \n ").is_empty());
    }

    #[test]
    fn numbers_and_name_runs_are_specifics_and_function_words_are_not() {
        assert_eq!(
            texts("The survey by Adaeze Okafor covered 14 agreements in 2019."),
            vec![
                (SpecificKind::Name, "Adaeze Okafor".into()),
                (SpecificKind::Number, "14".into()),
                (SpecificKind::Number, "2019".into()),
            ]
        );
        assert_eq!(
            texts("Hamilton, Madison and Jay wrote; Hume's Treatise followed."),
            vec![
                (SpecificKind::Name, "Madison".into()),
                (SpecificKind::Name, "Jay".into()),
                (SpecificKind::Name, "Hume".into()),
                (SpecificKind::Name, "Treatise".into()),
            ]
        );
        assert!(texts("it rained on the river all week.").is_empty());
    }

    /// The two shapes measured as false flags on X1 (see the module doc): a
    /// one-digit number, and a lone capitalised word opening the sentence. A
    /// name of two words opening it still counts, at its own range.
    #[test]
    fn a_one_digit_number_and_a_capitalised_first_word_are_not_specifics() {
        assert_eq!(
            texts("Finally, Book 3 of the Treatise turns to morals."),
            vec![
                (SpecificKind::Name, "Book".into()),
                (SpecificKind::Name, "Treatise".into()),
            ]
        );
        assert_eq!(texts("Section 2's argument fails."), vec![]);
        let s = "\"Adam Smith read it in 1740.";
        let got = sentence_specifics(s);
        assert_eq!(got[0].text, "Adam Smith");
        assert_eq!(got[0].range, 1..11, "the range skips the opening quote");
        assert_eq!(got[1].text, "1740");
    }

    #[test]
    fn a_specifics_range_names_it_in_the_sentence() {
        let s = "Ce café, dit Émile Zola en 1898.";
        for x in sentence_specifics(s) {
            let at: String = s.chars().skip(x.range.start).take(x.range.len()).collect();
            assert!(at.starts_with(x.text.split(' ').next().unwrap()), "{x:?}");
            assert_eq!(at.chars().count(), x.range.len());
        }
        assert_eq!(
            texts(s),
            vec![
                (SpecificKind::Name, "Émile Zola".into()),
                (SpecificKind::Number, "1898".into()),
            ]
        );
    }
}
