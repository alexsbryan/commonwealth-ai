// SPDX-License-Identifier: AGPL-3.0-or-later
//! A text's sentences and each sentence's specifics (the figures, dates,
//! quantities and names it asserts), found without a model: what
//! `POST /oicp/v1/verify` (`cmnwlth/docs/oicp-v0.6.md` §2.15) names on every
//! claim, hints at the fast tier with [`super::value_present_in_chunks`],
//! pairs against a passage's when the passage states one otherwise
//! ([`differing_specifics`]), and cuts from what its checker is shown
//! ([`without_citations`]).
//!
//! Both are shapes, not readings. A sentence ends at `.`, `!` or `?` before
//! whitespace and an uppercase letter, a digit or an opening quote, unless
//! the word before the stop is one capital (an initial, "J. S. Mill") or a
//! locator's or citation's abbreviation before a digit ("p. 4", "et al.
//! 2019"). A number is a word carrying a digit, with its currency sign and
//! percent sign, or a cardinal written in words ([`spelled`]); it is a
//! `Figure` (with a unit when a percent or currency marks it), a `Date` (a
//! year, a day beside a month; digits only), or a `Quantity` (a number
//! before the noun it counts), bounded when words before it say so ("more
//! than 50", [`bound`]). A name is a run of name-shaped words
//! ([`super::judge::cap_name`]), less the words that are never names
//! ([`super::judge::non_name_word`]), where a run that opens the sentence
//! counts only once it is two words long.
//!
//! Nothing inside a citation or a locator is a specific (§2.15): an
//! author-date citation (`(Okafor 2019)`, `(Okafor, 2019, p. 4)`), a citekey
//! (`[@okafor2019]`), a numbered reference (`[12]`), a locator (`(page 14)`,
//! `(p. 3)`, `(§ 4)`, `(chapter 2)`). A figure inside any other parentheses
//! is one: `(against 6.1 percent the year before)` names `6.1`.
//!
//! Two exclusions were measured on X1's draft over its Hume library (507
//! sentences, 2026-10-12): 87 of the 99 sentences the old fast tier flagged
//! were flagged only for a bare one-digit number ("Book 3") or for the
//! capitalised first word of the sentence ("Finally", "Similarly"). A bare
//! one-digit figure is still none; one with a unit, a counted noun or a month
//! beside it is one. The cost is a sentence that opens with a one-word name
//! ("Hume wrote…") offering that name to no check. Ranges are code points.

use std::collections::HashSet;
use std::ops::Range;

use super::judge::{cap_name, non_name_word};

pub use bound::FigureBound;

/// What a specific is (§2.15's closed set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecificKind {
    /// A number with or without a unit, a percentage, a currency amount.
    Figure,
    /// A year, or a day beside a month.
    Date,
    /// A number with the noun it counts.
    Quantity,
    /// A run of name-shaped words: a person, a place, a work.
    Name,
}

/// One specific a sentence asserts, as it is written there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specific {
    /// Figure, date, quantity or name.
    pub kind: SpecificKind,
    /// The specific as written: a number with its currency and percent
    /// signs, a name's words joined by one space.
    pub text: String,
    /// Code points of `text` in the sentence.
    pub range: Range<usize>,
    /// What a figure or a quantity counts in: a figure's `percent` or
    /// currency sign, a quantity's noun (lower-case, singular). `None` for a
    /// bare figure, a date and a name. Two specifics differ only when their
    /// kinds and units agree ([`differing_specifics`]).
    pub unit: Option<String>,
    /// A number's value in canonical digits, by which two specifics are
    /// compared: `48000` for "48,000" and for "forty-eight thousand".
    /// `None` for a name, and for a number compared as written ("1990s").
    pub value: Option<String>,
    /// The side of its number a figure's or a quantity's value lies on, read
    /// from the words before it ("more than 50"); `None` for a point, a date
    /// and a name ([`bound`]).
    pub bound: Option<FigureBound>,
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
    if j < chars.len() && chars[j].is_ascii_digit() && cites_before(chars, i) {
        return false;
    }
    j == chars.len()
        || chars[j].is_uppercase()
        || chars[j].is_ascii_digit()
        || matches!(chars[j], '"' | '\u{201c}' | '(' | '[')
}

/// Abbreviations a locator or a citation puts before a number: "p. 4",
/// "Vol. 2", "et al. 2019". Lower-case, without the stop.
const CITING_ABBREVIATIONS: &[&str] = &[
    "p", "pp", "ch", "chap", "vol", "fig", "sec", "no", "art", "para", "al", "ed", "eds", "cf",
];

/// Whether the word ending at the stop `i` is one of [`CITING_ABBREVIATIONS`].
fn cites_before(chars: &[char], i: usize) -> bool {
    let mut a = i;
    while a > 0 && chars[a - 1].is_alphabetic() {
        a -= 1;
    }
    let word: String = chars[a..i].iter().collect::<String>().to_lowercase();
    CITING_ABBREVIATIONS.contains(&word.as_str())
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

/// The specifics `sentence` asserts, in reading order. Nothing inside a
/// citation or a locator ([`cited_ranges`]) is one.
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
                unit: None,
                value: None,
                bound: None,
            });
        }
    }
    let cited = cited_ranges(sentence);
    let words = words_with_offsets(sentence);
    let mut out = Vec::new();
    let mut run: Option<Run> = None;
    let mut first = true;
    // Words a number written in words already took.
    let mut taken = 0;
    for (n, &(word, at)) in words.iter().enumerate() {
        if n < taken {
            continue;
        }
        let Some((trimmed, lead)) = trim_word(word) else {
            flush(&mut run, &mut out);
            continue;
        };
        let opens_sentence = std::mem::replace(&mut first, false);
        let a = at + lead;
        let within = |r: &Range<usize>| r.start <= a && a + trimmed.chars().count() <= r.end;
        if cited.iter().any(within) || word.trim_start_matches(['[', '(']).starts_with('@') {
            flush(&mut run, &mut out);
            continue;
        }
        let bare = strip_possessive(trimmed);
        if bare.chars().any(|c| c.is_ascii_digit()) {
            flush(&mut run, &mut out);
            let around = Around {
                word,
                lead,
                possessive: bare.len() != trimmed.len(),
                before: n.checked_sub(1).and_then(|i| words.get(i)).map(|w| w.0),
                after: words.get(n + 1).map(|w| w.0),
                after2: words.get(n + 2).map(|w| w.0),
            };
            if let Some(s) = number_specific(bare, a, &around) {
                out.push(s);
            }
            continue;
        }
        if let Some((took, s)) = spelled::word_number(sentence, &words, n, opens_sentence) {
            flush(&mut run, &mut out);
            out.extend(s);
            taken = n + took;
            continue;
        }
        // A month beside a number is part of a written date, never a name.
        let has_digit = |i: Option<&(&str, usize)>| {
            i.is_some_and(|w| w.0.contains(|c: char| c.is_ascii_digit()))
        };
        if is_month(&bare.to_lowercase())
            && (has_digit(n.checked_sub(1).and_then(|i| words.get(i)))
                || has_digit(words.get(n + 1)))
        {
            flush(&mut run, &mut out);
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
    bound::read_bounds(sentence, &mut out);
    out
}

/// A number word and its neighbours, as written.
struct Around<'a> {
    word: &'a str,
    /// Code points trimmed from the word's front.
    lead: usize,
    possessive: bool,
    before: Option<&'a str>,
    after: Option<&'a str>,
    after2: Option<&'a str>,
}

/// Words that follow a number without being what it counts.
const NOT_COUNTED: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "but",
    "by",
    "for",
    "from",
    "in",
    "into",
    "is",
    "it",
    "its",
    "more",
    "less",
    "of",
    "on",
    "or",
    "out",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "these",
    "this",
    "those",
    "to",
    "was",
    "were",
    "which",
    "who",
    "with",
    "per",
    "percent",
    "percentage",
    "pct",
    "times",
];

const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

const CURRENCY: &[char] = &['$', '£', '€', '¥'];

/// A neighbouring word, lower-case, its punctuation trimmed.
fn plain(w: Option<&str>) -> String {
    w.and_then(trim_word)
        .map_or_else(String::new, |(t, _)| t.to_lowercase())
}

fn is_month(w: &str) -> bool {
    MONTHS.contains(&w)
}

/// A year: four digits from 1000 to 2999, or a decade ("1990s").
fn is_year(t: &str) -> bool {
    let digits = t.strip_suffix('s').unwrap_or(t);
    digits.len() == 4
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (b'1'..=b'2').contains(&digits.as_bytes()[0])
}

/// The specific a number word `bare` (at code point `a`) asserts, with its
/// currency and percent signs, or none: a bare one-digit figure.
fn number_specific(bare: &str, a: usize, w: &Around<'_>) -> Option<Specific> {
    // The signs `trim_word` cut from the word, taken back.
    let front: Vec<char> = w.word.chars().take(w.lead).collect();
    let currency = front.last().filter(|c| CURRENCY.contains(c)).copied();
    let back = w.word.chars().nth(w.lead + bare.chars().count());
    let percent_sign = back == Some('%') && !w.possessive;
    let start = a - usize::from(currency.is_some());
    let mut text: String = currency.into_iter().collect();
    text.push_str(bare);
    if percent_sign {
        text.push('%');
    }
    let range = start..start + text.chars().count();
    let (next, prev) = (plain(w.after), plain(w.before));
    let noun = || (!w.possessive && !bare.contains('.')).then(|| counted_noun(w.after));
    let (kind, unit) = if percent_sign || percent_follows(w.after, w.after2) {
        (SpecificKind::Figure, Some("percent".to_string()))
    } else if let Some(c) = currency {
        (SpecificKind::Figure, Some(c.to_string()))
    } else if is_year(bare) {
        (SpecificKind::Date, None)
    } else if bare.len() <= 2 && (is_month(&next) || is_month(&prev)) {
        (SpecificKind::Date, None)
    } else if let Some(Some(noun)) = noun() {
        (SpecificKind::Quantity, Some(noun))
    } else {
        (SpecificKind::Figure, None)
    };
    if kind == SpecificKind::Figure && unit.is_none() && text.chars().count() < 2 {
        return None;
    }
    Some(Specific {
        kind,
        value: super::number_presence::numeric_value(&text),
        text,
        range,
        unit,
        bound: None,
    })
}

/// Whether the words after a number mark it a percentage: "percent",
/// "percentage", "pct", "per cent".
fn percent_follows(after: Option<&str>, after2: Option<&str>) -> bool {
    let next = plain(after);
    matches!(next.as_str(), "percent" | "percentage" | "pct")
        || (next == "per" && plain(after2) == "cent")
}

/// The noun a number counts, singular, when the word after it is one: lower
/// case as written and not one of [`NOT_COUNTED`].
fn counted_noun(after: Option<&str>) -> Option<String> {
    let next = plain(after);
    (!next.is_empty()
        && next.chars().all(char::is_lowercase)
        && !NOT_COUNTED.contains(&next.as_str())
        && after.is_some_and(|x| x.starts_with(|c: char| c.is_lowercase())))
    .then(|| singular(&next))
}

/// A counted noun's singular, by shape: "sailings" and "sailing" count one thing.
fn singular(noun: &str) -> String {
    match noun.strip_suffix('s') {
        Some(stem) if stem.len() >= 3 && !stem.ends_with('s') => stem.to_string(),
        _ => noun.to_string(),
    }
}

/// Code-point ranges of `sentence` that are citations or locators, brackets
/// included: parentheses holding an author-date citation or a locator, and
/// square brackets holding a citekey (`@`) or reference numbers.
pub(crate) fn cited_ranges(sentence: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = sentence.chars().collect();
    let mut open: Vec<(char, usize)> = Vec::new();
    let mut out = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '(' | '[' => open.push((c, i)),
            ')' | ']' => {
                let Some(at) = open.iter().rposition(|(o, _)| (*o == '(') == (c == ')')) else {
                    continue;
                };
                let (o, start) = open[at];
                open.truncate(at);
                let inner: String = chars[start + 1..i].iter().collect();
                let cites = if o == '(' {
                    is_locator(&inner) || is_author_date(&inner)
                } else {
                    inner.contains('@')
                        || (inner.chars().any(|c| c.is_ascii_digit())
                            && inner
                                .chars()
                                .all(|c| c.is_ascii_digit() || " ,;-\u{2013}".contains(c)))
                };
                if cites {
                    out.push(start..i + 1);
                }
            }
            _ => {}
        }
    }
    out
}

/// `sentence` with its citations and locators ([`cited_ranges`], the one
/// recogniser) taken out, and how many were: what a model judging the
/// sentence's content is shown, since a source backs what a claim says, not
/// who it cites (§2.15). The whitespace at each cut is tidied — one space
/// between words, none before closing punctuation; the rest is untouched.
pub fn without_citations(sentence: &str) -> (String, usize) {
    let mut cuts = cited_ranges(sentence);
    if cuts.is_empty() {
        return (sentence.to_string(), 0);
    }
    // A citation inside a cited parenthesis is cut with it.
    cuts.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    cuts.dedup_by(|inner, outer| inner.end <= outer.end);
    let chars: Vec<char> = sentence.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    for r in &cuts {
        out.extend(&chars[i..r.start]);
        out.truncate(out.trim_end().len());
        i = r.end;
        while chars.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        if !out.is_empty() && chars.get(i).is_some_and(|c| !".,;:!?)]".contains(*c)) {
            out.push(' ');
        }
    }
    out.extend(&chars[i..]);
    (out, cuts.len())
}

/// Words that open a locator: "(page 14)", "(p. 3)", "(§ 4)", "(chapter 2)".
const LOCATORS: &[&str] = &[
    "p",
    "pp",
    "page",
    "pages",
    "§",
    "§§",
    "section",
    "sections",
    "sec",
    "chapter",
    "chapters",
    "chap",
    "ch",
    "vol",
    "volume",
    "book",
    "part",
    "fig",
    "figure",
    "table",
    "line",
    "lines",
    "para",
    "paragraph",
    "art",
    "article",
    "note",
    "n",
    "col",
    "column",
    "folio",
    "fol",
];

/// A locator: an opening word of [`LOCATORS`] and then only numbers, roman
/// numerals and range marks.
fn is_locator(inner: &str) -> bool {
    let inner = inner.trim();
    let (head, rest) = match inner.strip_prefix('§') {
        Some(rest) => ("§", rest),
        None => inner.split_at(
            inner
                .find(|c: char| !c.is_alphabetic())
                .unwrap_or(inner.len()),
        ),
    };
    let numbered = |t: &str| {
        t.chars()
            .any(|c| c.is_ascii_digit() || "ivxlcIVXLC".contains(c))
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || " .,-\u{2013}§".contains(c))
            && t.split_whitespace().all(|w| {
                w.chars().any(|c| c.is_ascii_digit())
                    || w.trim_matches(|c: char| !c.is_alphanumeric())
                        .chars()
                        .all(|c| "ivxlcIVXLC".contains(c))
            })
    };
    LOCATORS.contains(&head.to_lowercase().as_str()) && numbered(rest.trim_start_matches('.'))
}

/// Words an author-date citation may hold beside names and years.
const CITATION_WORDS: &[&str] = &[
    "et", "al", "and", "&", "see", "cf", "also", "e.g", "eg", "p", "pp", "ed", "eds", "in", "nd",
    "n.d",
];

/// An author-date citation: each `;`-separated part holds a name-shaped word
/// that is not a month, and a year (`2019`, `2019a`, `n.d.`), and nothing
/// else but [`CITATION_WORDS`] and page numbers.
fn is_author_date(inner: &str) -> bool {
    inner.split(';').all(|part| {
        let mut name = false;
        let mut year = false;
        for t in part.split([' ', ',']).filter(|t| !t.is_empty()) {
            let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != '&');
            let lower = t.to_lowercase();
            let digits = t.trim_end_matches(|c: char| c.is_ascii_lowercase());
            if is_year(digits) || lower == "n.d" || lower == "nd" {
                year = true;
            } else if t.starts_with(|c: char| c.is_uppercase()) && !is_month(&lower) {
                name = true;
            } else if !(CITATION_WORDS.contains(&lower.as_str())
                || t.is_empty()
                || t.chars()
                    .all(|c| c.is_ascii_digit() || c == '-' || c == '\u{2013}'))
            {
                return false;
            }
        }
        name && year
    })
}

/// The specifics of `claim` that a passage holding `passage` (its
/// specifics, found in `passage_text`) states otherwise, each paired with
/// the passage's specific of the same kind and unit (§2.15, "contradicted
/// names what differs"). Read by value (`3.2%` is `3.2 per cent`), so a
/// claim's figure the passage carries in any form is never a difference.
///
/// A bounded figure or quantity ("more than 50 years") differs only from one
/// no value of its range meets ([`bound`]): "more than a hundred years" does
/// not state "more than 50" otherwise, and "fewer than 50" it does.
///
/// A difference in a figure or a quantity counts only where the claim's
/// dates are not also stated otherwise: two periods' figures differing is
/// not a contradiction. A date stated otherwise counts only beside something
/// of the claim the passage does carry (a name, a figure): "Mill wrote On
/// Liberty in 1858" against "On Liberty appeared in 1859".
pub fn differing_specifics(
    claim: &[Specific],
    passage: &[Specific],
    passage_text: &str,
) -> Vec<(Specific, Specific)> {
    let value = |s: &Specific| s.value.clone().unwrap_or_else(|| s.text.to_lowercase());
    let counted = |s: &&Specific| s.kind != SpecificKind::Name;
    let passage_values: HashSet<String> = passage.iter().filter(counted).map(value).collect();
    let claim_values: HashSet<String> = claim.iter().filter(counted).map(value).collect();
    let held = |s: &Specific| match s.kind {
        SpecificKind::Name => passage_text.to_lowercase().contains(&s.text.to_lowercase()),
        _ => passage_values.contains(&value(s)),
    };
    let pairs: Vec<(Specific, Specific)> = claim
        .iter()
        .filter(counted)
        .filter(|s| !held(*s))
        .filter_map(|s| {
            let alike = |p: &&Specific| p.kind == s.kind && p.unit == s.unit;
            if let Some(met) = passage
                .iter()
                .filter(alike)
                .find(|p| bound::ranges_meet(s, p) == Some(true))
            {
                tracing::debug!(target: "grounding_gate", event = "differing_bound_met", claim = %s.text, passage = %met.text, "a bounded figure the passage's meets: not stated otherwise");
                return None;
            }
            let other = passage
                .iter()
                .filter(alike)
                .find(|p| !claim_values.contains(&value(*p)))?;
            Some((s.clone(), other.clone()))
        })
        .collect();
    let (dates, values): (Vec<_>, Vec<_>) = pairs
        .into_iter()
        .partition(|(s, _)| s.kind == SpecificKind::Date);
    if !values.is_empty() {
        if dates.is_empty() {
            return values;
        }
        tracing::debug!(target: "grounding_gate", event = "differing_other_period", values = values.len(), dates = dates.len(), "a figure and a date both differ: another period's figure, not a contradiction");
        return Vec::new();
    }
    if dates.is_empty() {
        return Vec::new();
    }
    let anchored = claim
        .iter()
        .any(|s| s.kind != SpecificKind::Date && held(s));
    if !anchored {
        tracing::debug!(target: "grounding_gate", event = "differing_date_unanchored", dates = dates.len(), "only a date differs and nothing else of the claim is carried: not about the same thing");
        return Vec::new();
    }
    dates
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

// A figure's bound ("more than 50"), and whether two bounded specifics meet.
#[path = "specifics_bound.rs"]
mod bound;

// Cardinals written in words, read by the runtime's one number-word reader.
#[path = "specifics_spelled.rs"]
mod spelled;

// The finder's tests live in a sibling file, keeping this one under its
// size ceiling. `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "specifics_tests.rs"]
mod tests;
