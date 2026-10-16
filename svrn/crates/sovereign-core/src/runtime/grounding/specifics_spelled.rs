// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cardinals written in words as specifics (§2.15): "forty-eight thousand
//! pins" is a quantity of 48000 like "48,000 pins", "a hundred years" one of
//! 100, "twenty percent" a figure. Prose writes its numbers in words, and a
//! claim or a source sentence whose figure is in words otherwise offers no
//! specific to check or to differ (live 2026-10-15, Wealth of Nations).
//!
//! The value is the runtime's one number-word reader's
//! ([`crate::runtime::number_words::number_phrase`]). The range is the words
//! as written, an article before a scale word included ("a hundred"). What is
//! none: an ordinal or a fraction ("the first settlement", "a tenth part");
//! a lone "one", which is a pronoun or a determiner far more often than a
//! count ("one of the workmen", "one must admit"); a bare number word under
//! ten, as a bare digit is; a capitalised number word inside the sentence,
//! which is a name's ("the Seven Years War"); a scale word after digits,
//! which is their magnitude ("$32 billion"). A number written in words is
//! never a date. In an alternation ("two or three distinct operations") each
//! end counts what the last counts, so both are quantities.

use super::{counted_noun, percent_follows, plain, trim_word, Specific, SpecificKind};
use crate::runtime::number_words::{
    is_number_word, magnitude_of_digits, number_phrase, number_word_value, NumberPhrase,
};

/// A phrase read at a word: its reading, the index of its last word, and
/// whether punctuation closes that word (nothing after it is counted).
struct PhraseRead {
    phrase: NumberPhrase,
    last: usize,
    closed: bool,
}

/// The number phrase whose first word is `words[from]`, stopping at a word
/// opened by punctuation and after one closed by it.
fn read_at(words: &[(&str, usize)], from: usize) -> Option<PhraseRead> {
    let mut run: Vec<&str> = Vec::new();
    let mut closed = Vec::new();
    for (k, &(w, _)) in words.iter().enumerate().skip(from) {
        let Some((t, lead)) = trim_word(w) else { break };
        if (k > from && lead > 0) || !(is_number_word(t) || t.eq_ignore_ascii_case("and")) {
            break;
        }
        run.push(t);
        let shut = lead + t.chars().count() < w.chars().count();
        closed.push(shut);
        if shut {
            break;
        }
    }
    let phrase = number_phrase(&run)?;
    Some(PhraseRead {
        last: from + phrase.words - 1,
        closed: closed[phrase.words - 1],
        phrase,
    })
}

/// The number written in words that `words[n]` opens in `sentence`, if
/// any: how many words it takes, and the specific it asserts, which may be
/// none (see the module doc).
pub(super) fn word_number(
    sentence: &str,
    words: &[(&str, usize)],
    n: usize,
    opens_sentence: bool,
) -> Option<(usize, Option<Specific>)> {
    let (first, lead) = trim_word(words[n].0)?;
    if !opens_sentence && first.starts_with(char::is_uppercase) {
        return None;
    }
    let article = first.eq_ignore_ascii_case("a") && !tail_closed(words, n);
    let from = n + usize::from(article);
    if article
        && !words
            .get(from)
            .and_then(|w| trim_word(w.0))
            .is_some_and(|(t, l)| l == 0 && number_word_value(t).is_some_and(|w| w.scale))
    {
        return None;
    }
    let read = read_at(words, from)?;
    let took = read.last + 1 - n;
    let start = words[n].1 + lead;
    let (last, last_lead) = trim_word(words[read.last].0)?;
    let end = words[read.last].1 + last_lead + last.chars().count();
    let text: String = sentence.chars().skip(start).take(end - start).collect();
    let p = read.phrase;
    let skip = |reason: &str| {
        tracing::debug!(target: "grounding_gate", event = "word_figure_skipped", text = %text, reason, "a number written in words read as no specific");
        Some((took, None))
    };
    if !article && magnitude_of_digits(n.checked_sub(1).map(|k| words[k].0), first) {
        return skip("magnitude_of_digits");
    }
    if p.ordinal {
        return skip("ordinal");
    }
    if p.value == 1 && !p.scaled {
        return skip("lone_one");
    }
    // What counts the number: the words after it, or after the last end of
    // an alternation it opens ("two or three distinct operations").
    let mut tail = (read.last, read.closed);
    if !read.closed && plain(words.get(read.last + 1).map(|w| w.0)) == "or" {
        if let Some(other) = read_at(words, read.last + 2).filter(|o| !o.phrase.ordinal) {
            tail = (other.last, other.closed);
        }
    }
    let (after, after2) = match tail {
        (_, true) => (None, None),
        (k, false) => (words.get(k + 1).map(|w| w.0), words.get(k + 2).map(|w| w.0)),
    };
    let (kind, unit) = if percent_follows(after, after2) {
        (SpecificKind::Figure, Some("percent".to_string()))
    } else if let Some(noun) = counted_noun(after) {
        (SpecificKind::Quantity, Some(noun))
    } else if p.value < 10 {
        return skip("bare_under_ten");
    } else {
        (SpecificKind::Figure, None)
    };
    tracing::debug!(target: "grounding_gate", event = "word_figure_read", text = %text, value = p.value, kind = ?kind, unit = ?unit, "a number written in words read as a specific");
    Some((
        took,
        Some(Specific {
            kind,
            text,
            range: start..end,
            unit,
            value: Some(p.value.to_string()),
            bound: None,
        }),
    ))
}

/// Whether punctuation closes `words[k]`.
fn tail_closed(words: &[(&str, usize)], k: usize) -> bool {
    words.get(k).is_some_and(|&(w, _)| {
        trim_word(w).is_some_and(|(t, l)| l + t.chars().count() < w.chars().count())
    })
}
