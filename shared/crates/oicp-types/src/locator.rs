// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a span stands (v0.6 §2.13): [`SpanLocator`], the closed set of
//! places an extraction can name, and [`HeadingOutline`], the plain-text
//! extractor's rule for the heading path a span stands under.
//!
//! The rule is here, beside the type, so the reference host's extractor and
//! any test host read headings by one implementation; the conformance
//! fixture's expected paths are data, so a host that agrees with this code
//! and not with the spec still fails.

use serde::{Deserialize, Serialize};

/// Where extraction found a span: the place a reader would cite it by. A
/// hint beside the range, which stays the span's address. A kind outside
/// the set is a decode error, never a default; adding one is a spec
/// revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpanLocator {
    /// A page of a paginated source.
    Page {
        /// The 1-based page the span starts on.
        page: u32,
    },
    /// The headings the span stands under.
    HeadingPath {
        /// Outermost first, each as its paragraph reads; never empty.
        headings: Vec<String>,
    },
    /// A transcript turn.
    Timestamp {
        /// The start of the turn the span starts in.
        milliseconds: u64,
        /// Its speaker when the transcript names one; absent, never guessed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
    },
    /// A record of a file of many.
    Row {
        /// The 1-based record the span is cut from.
        row: u64,
    },
}

/// The words that open a heading, in capitals or with a capital initial
/// (§2.13). How deep each one sits is the text's own: the order its
/// headings first use them.
pub const PART_WORDS: &[&str] = &["BOOK", "PART", "CHAPTER", "SECTION", "VOLUME"];

/// The longest paragraph, in code points with its whitespace runs read as
/// one space, that is read as a heading (§2.13).
pub const HEADING_MAX_CHARS: usize = 300;

/// A plain text's headings by §2.13's rule: where each starts and the path
/// it opens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadingOutline {
    heads: Vec<(u64, Vec<String>)>,
}

impl HeadingOutline {
    /// The headings of `text`, in text order.
    pub fn of(text: &str) -> Self {
        let found: Vec<(u64, String, String)> = paragraphs(text)
            .into_iter()
            .filter_map(|(start, p)| heading_word(&p).map(|w| (start, w, p)))
            .collect();
        // Part words nest in the order the headings first use them, so one
        // pass both ranks a word and places its heading.
        let mut levels: Vec<String> = Vec::new();
        let mut open: Vec<(usize, String)> = Vec::new();
        let mut heads = Vec::new();
        for (start, word, p) in found {
            let level = match levels.iter().position(|w| *w == word) {
                Some(l) => l,
                None => {
                    levels.push(word);
                    levels.len() - 1
                }
            };
            open.retain(|(l, _)| *l < level);
            open.push((level, p));
            heads.push((start, open.iter().map(|(_, h)| h.clone()).collect()));
        }
        Self { heads }
    }

    /// Each heading's first code point and the path it opens, in text order.
    pub fn heads(&self) -> &[(u64, Vec<String>)] {
        &self.heads
    }

    /// The headings as the places a host keeps beside the stored text: each
    /// heading's first code point and the path it opens. Empty for a text
    /// with none, so nothing is placed and no span is located.
    pub fn placed(&self) -> PlacedLocators {
        PlacedLocators(
            self.heads
                .iter()
                .map(|(at, path)| PlacedLocator {
                    at: *at,
                    locator: SpanLocator::HeadingPath {
                        headings: path.clone(),
                    },
                })
                .collect(),
        )
    }

    /// The heading path at code point `at`: the one the last heading
    /// starting at or before it opened. `None` before the first heading and
    /// in a text with none.
    pub fn at(&self, at: u64) -> Option<SpanLocator> {
        self.heads
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .map(|(_, path)| SpanLocator::HeadingPath {
                headings: path.clone(),
            })
    }
}

/// The paragraphs of `text` (runs of lines that are not whitespace alone),
/// each with its first non-whitespace code point and its words joined by
/// single spaces.
fn paragraphs(text: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut current: Option<(u64, Vec<&str>)> = None;
    let mut at = 0u64;
    for line in text.split_inclusive('\n') {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            if let Some((start, w)) = current.take() {
                out.push((start, w.join(" ")));
            }
        } else {
            let lead = line.chars().take_while(|c| c.is_whitespace()).count() as u64;
            current
                .get_or_insert_with(|| (at + lead, Vec::new()))
                .1
                .extend(words);
        }
        at += line.chars().count() as u64;
    }
    if let Some((start, w)) = current {
        out.push((start, w.join(" ")));
    }
    out
}

/// The part word, in capitals, when `p` (words joined by single spaces) is a
/// heading: it opens with a part word and a numeral and a stop, is at most
/// [`HEADING_MAX_CHARS`] long, and opens no second part (a contents line).
fn heading_word(p: &str) -> Option<String> {
    if p.chars().count() > HEADING_MAX_CHARS {
        return None;
    }
    let words: Vec<&str> = p.split(' ').collect();
    let opens = |i: usize| -> Option<&'static str> {
        let word = part_word(words.get(i)?)?;
        numbered(words.get(i + 1)?).then_some(word)
    };
    let word = opens(0)?;
    if (1..words.len()).any(|i| opens(i).is_some()) {
        return None;
    }
    Some(word.to_string())
}

/// `w` as a part word, in capitals (`BOOK`) or with a capital initial
/// (`Book`).
fn part_word(w: &str) -> Option<&'static str> {
    PART_WORDS
        .iter()
        .copied()
        .find(|p| w == *p || w == format!("{}{}", &p[..1], p[1..].to_lowercase()))
}

/// `w` opens with a numeral (Arabic digits, or capital Roman numerals) and
/// a stop right after it: `I.`, `12.`, `II.—Of…`.
fn numbered(w: &str) -> bool {
    let Some(stop) = w.find('.') else {
        return false;
    };
    let n = &w[..stop];
    !n.is_empty()
        && (n.bytes().all(|b| b.is_ascii_digit()) || n.bytes().all(|b| b"IVXLCDM".contains(&b)))
}

/// Where each place starts in a stored text, in text order: what a host keeps
/// beside a text so a span's [`SpanLocator`] is read back at serve time, not
/// re-derived from a source it no longer holds. One shape for every kind
/// (a transcript's turns now, a plain text's headings next), serialised as
/// JSON by the writer and parsed by the reader.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlacedLocators(pub Vec<PlacedLocator>);

/// One place: its first code point and the locator spans starting at or after
/// it (until the next place) carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedLocator {
    /// The place's first code point in the stored text.
    pub at: u64,
    /// The locator of a span that starts in it.
    pub locator: SpanLocator,
}

impl PlacedLocators {
    /// The locator at code point `at`: the last place starting at or before
    /// it. `None` before the first place.
    pub fn at(&self, at: u64) -> Option<SpanLocator> {
        self.0
            .iter()
            .rev()
            .find(|p| p.at <= at)
            .map(|p| p.locator.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(o: &HeadingOutline, text: &str, quote: &str) -> Option<Vec<String>> {
        let at = text[..text.find(quote).unwrap()].chars().count() as u64;
        match o.at(at)? {
            SpanLocator::HeadingPath { headings } => Some(headings),
            other => panic!("{other:?}"),
        }
    }

    /// The shape of a Gutenberg book: a contents paragraph that names every
    /// part (never a heading), headings in capitals over several levels, a
    /// lower level first met after a higher one, and prose that merely
    /// opens with a part word.
    #[test]
    fn a_span_stands_under_the_headings_open_at_its_start() {
        let text = "Contents\n\nBOOK I. OF LABOUR. CHAPTER I. OF DIVISION.\n\nA preface.\n\n\
                    BOOK I. OF LABOUR.\n\nCHAPTER I. OF DIVISION.\n\nPins are made.\n\n\
                    PART II.—Of wages.\n\nWages rise.\n\nCHAPTER II. OF MONEY.\n\n\
                    Chapter and verse.\n\nCoins circulate.\n\nBOOK II. OF STOCK.\n\nStock is kept.\n";
        let o = HeadingOutline::of(text);
        assert_eq!(path(&o, text, "A preface."), None);
        let book = "BOOK I. OF LABOUR.".to_string();
        let ch1 = "CHAPTER I. OF DIVISION.".to_string();
        assert_eq!(
            path(&o, text, "Pins"),
            Some(vec![book.clone(), ch1.clone()])
        );
        assert_eq!(
            path(&o, text, "Wages rise"),
            Some(vec![book.clone(), ch1, "PART II.—Of wages.".into()])
        );
        let ch2 = vec![book, "CHAPTER II. OF MONEY.".to_string()];
        assert_eq!(path(&o, text, "Coins"), Some(ch2.clone()));
        assert_eq!(path(&o, text, "Chapter and verse"), Some(ch2));
        assert_eq!(
            path(&o, text, "Stock is kept"),
            Some(vec!["BOOK II. OF STOCK.".to_string()])
        );
        assert_eq!(
            path(&o, text, "CHAPTER I. OF DIVISION.\n\nPins").map(|p| p.len()),
            Some(2)
        );
    }

    #[test]
    fn a_text_with_no_headings_has_no_locator_anywhere() {
        let text = "Tuesday. The ferry ran late.\n\nWednesday. Chapter 3 of the log was lost.\n";
        let o = HeadingOutline::of(text);
        assert!(o.heads().is_empty());
        assert_eq!(o.at(0), None);
        assert_eq!(o.at(40), None);
    }

    /// What a host keeps beside a text answers every code point as the
    /// outline does, after a JSON round trip; a text with no headings
    /// places nothing.
    #[test]
    fn placed_headings_survive_json_and_answer_as_the_outline_does() {
        let text = "Preface.\n\nBOOK I. OF LABOUR.\n\nCHAPTER I. OF DIVISION.\n\nPins.\n\n\
                    BOOK II. OF STOCK.\n\nStock.\n";
        let o = HeadingOutline::of(text);
        let back: PlacedLocators =
            serde_json::from_str(&serde_json::to_string(&o.placed()).unwrap()).unwrap();
        for at in 0..text.chars().count() as u64 {
            assert_eq!(back.at(at), o.at(at), "code point {at}");
        }
        assert!(HeadingOutline::of("Tuesday. No headings.\n")
            .placed()
            .0
            .is_empty());
    }

    #[test]
    fn a_heading_wraps_lines_and_is_read_as_one_spaced_paragraph() {
        let text = "  CHAPTER  IV.\n  OF THE ORIGIN\nOF MONEY.\n\nMoney.";
        let o = HeadingOutline::of(text);
        assert_eq!(
            o.heads(),
            &[(2, vec!["CHAPTER IV. OF THE ORIGIN OF MONEY.".to_string()])]
        );
    }

    #[test]
    fn locators_travel_tagged_by_kind_and_refuse_an_unknown_kind() {
        let t = SpanLocator::Timestamp {
            milliseconds: 4000,
            speaker: None,
        };
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"kind":"timestamp","milliseconds":4000}"#
        );
        let h: SpanLocator =
            serde_json::from_str(r#"{"kind":"heading_path","headings":["BOOK I."]}"#).unwrap();
        assert_eq!(
            h,
            SpanLocator::HeadingPath {
                headings: vec!["BOOK I.".into()]
            }
        );
        assert!(serde_json::from_str::<SpanLocator>(r#"{"kind":"line","line":3}"#).is_err());
    }
}
