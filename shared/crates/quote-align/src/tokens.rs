// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tokens of a normalised text, and the parse of a quotation into tokens and
//! editorial gaps.

use std::ops::Range;

use crate::norm::NormText;

/// One token: a maximal run of alphanumerics (a word), or one other
/// non-space character. Ranges are into the normalised characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) norm: Range<usize>,
    pub(crate) word: bool,
}

pub(crate) fn tokenize(text: &NormText) -> Vec<Token> {
    let chars = text.chars();
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == ' ' {
            at += 1;
        } else if c.is_alphanumeric() {
            let start = at;
            while at < chars.len() && chars[at].is_alphanumeric() {
                at += 1;
            }
            out.push(Token {
                norm: start..at,
                word: true,
            });
        } else {
            out.push(Token {
                norm: at..at + 1,
                word: false,
            });
            at += 1;
        }
    }
    out
}

/// What a quotation is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// A token to align.
    Tok(Token),
    /// Editorial text standing for some stretch of the source.
    Gap { kind: GapKind, norm: Range<usize> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GapKind {
    /// An ellipsis: any stretch, free.
    Elide,
    /// Square-bracketed text, with its words (brackets removed, lower
    /// case): it stands for the shortest stretch holding as many of them as
    /// the source has, so "[The committee]" reaches back to "The Joint
    /// Committee" and "[sic]" stands for nothing.
    Bracket { words: Vec<Vec<char>> },
}

/// Split a normalised quotation into tokens and gaps.
///
/// Three or more consecutive `.` tokens are an ellipsis. A `[`…`]` group is a
/// bracket, widened over the word characters it touches, so "[T]he" is one
/// editorial unit standing for "The". An unclosed `[` is ordinary
/// punctuation. Ellipses at either edge elide text outside the quotation and
/// are dropped.
pub(crate) fn parse_quote(text: &NormText) -> Vec<Item> {
    let chars = text.chars();
    let toks = tokenize(text);
    let is = |t: &Token, c: char| !t.word && chars[t.norm.start] == c;
    let mut items: Vec<Item> = Vec::new();
    let mut at = 0;
    while at < toks.len() {
        if is(&toks[at], '.') {
            let run = toks[at..].iter().take_while(|t| is(t, '.')).count();
            if run >= 3 {
                let norm = toks[at].norm.start..toks[at + run - 1].norm.end;
                items.push(Item::Gap {
                    kind: GapKind::Elide,
                    norm,
                });
                at += run;
                continue;
            }
        }
        if is(&toks[at], '[') {
            if let Some(close) = (at + 1..toks.len()).find(|&k| is(&toks[k], ']')) {
                let mut start = toks[at].norm.start;
                while let Some(Item::Tok(prev)) = items.last() {
                    if !(prev.word && prev.norm.end == start) {
                        break;
                    }
                    start = prev.norm.start;
                    items.pop();
                }
                let mut end = toks[close].norm.end;
                let mut next = close + 1;
                while next < toks.len() && toks[next].word && toks[next].norm.start == end {
                    end = toks[next].norm.end;
                    next += 1;
                }
                let words = bracket_words(&chars[start..end]);
                items.push(Item::Gap {
                    kind: GapKind::Bracket { words },
                    norm: start..end,
                });
                at = next;
                continue;
            }
        }
        items.push(Item::Tok(toks[at].clone()));
        at += 1;
    }
    let is_elide = |i: &Item| {
        matches!(
            i,
            Item::Gap {
                kind: GapKind::Elide,
                ..
            }
        )
    };
    while items.last().is_some_and(is_elide) {
        items.pop();
    }
    let lead = items.iter().take_while(|i| is_elide(i)).count();
    items.drain(..lead);
    items
}

/// The words of a bracketed unit with the brackets taken out, lower-cased:
/// "[T]he" is "the".
fn bracket_words(unit: &[char]) -> Vec<Vec<char>> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    for &c in unit.iter().filter(|c| !matches!(c, '[' | ']')) {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::norm::norm_v0;

    fn shape(q: &str) -> Vec<String> {
        let t = norm_v0(q);
        let s = t.to_string();
        let slice = |r: &Range<usize>| s.chars().skip(r.start).take(r.len()).collect::<String>();
        parse_quote(&t)
            .iter()
            .map(|i| match i {
                Item::Tok(tok) => slice(&tok.norm),
                Item::Gap {
                    kind: GapKind::Elide,
                    norm,
                } => format!("<elide {}>", slice(norm)),
                Item::Gap {
                    kind: GapKind::Bracket { words },
                    norm,
                } => format!("<bracket/{} {}>", words.len(), slice(norm)),
            })
            .collect()
    }

    #[test]
    fn words_and_punctuation_are_tokens() {
        assert_eq!(
            shape("don't, she said"),
            ["don", "'", "t", ",", "she", "said"]
        );
    }

    #[test]
    fn an_interior_ellipsis_is_a_gap_and_an_edge_one_is_dropped() {
        assert_eq!(
            shape("...the river \u{2026} its own . . . counsel..."),
            [
                "the",
                "river",
                "<elide ...>",
                "its",
                "own",
                "<elide . . .>",
                "counsel"
            ]
        );
        assert_eq!(
            shape("end. two"),
            ["end", ".", "two"],
            "a period is a period"
        );
    }

    #[test]
    fn a_bracket_takes_the_word_characters_it_touches() {
        assert_eq!(shape("[T]he river"), ["<bracket/1 [T]he>", "river"]);
        assert_eq!(
            shape("the [joint committee] voted"),
            ["the", "<bracket/2 [joint committee]>", "voted"]
        );
        assert_eq!(shape("an [ open"), ["an", "[", "open"], "unclosed");
        assert_eq!(shape("[sic]"), ["<bracket/1 [sic]>"]);
    }
}
