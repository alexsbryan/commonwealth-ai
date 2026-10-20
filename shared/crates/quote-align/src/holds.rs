// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether a text holds a quotation's words, cheaply: which stored texts a
//! host must align a quotation against so that one standing there word for
//! word is found there (OICP v0.6 §2.18, PAL1).

use crate::norm::norm_v0;
use crate::tokens::{parse_quote, Item};

/// Whether `text` holds the longest gap-free run of `quote`'s words word for
/// word: those words in order, nothing but marks and spaces between them,
/// compared case-folded. A text that holds the quotation word for word,
/// punctuation aside, always passes; one that passes may still align
/// otherwise, which is the aligner's to say.
///
/// One substring search for the run's longest word (each of its lower,
/// capitalised and upper case forms), then a walk out from each place it
/// stands, so a text the word is not in costs one search. `text` is read as
/// stored, not normalised: a word composed differently there (NFD), or split
/// by a hyphen at a line end, is not found by this, and the host's lexical
/// candidates are all that can find it.
pub fn holds_words(text: &str, quote: &str) -> bool {
    let norm = norm_v0(quote);
    let chars = norm.chars();
    let mut runs: Vec<Vec<String>> = vec![Vec::new()];
    for item in parse_quote(&norm) {
        match item {
            Item::Tok(t) if t.word => {
                let word: String = chars[t.norm]
                    .iter()
                    .flat_map(|c| c.to_lowercase())
                    .collect();
                runs.last_mut().expect("one run at least").push(word);
            }
            Item::Tok(_) => {}
            Item::Gap { .. } => runs.push(Vec::new()),
        }
    }
    let Some(run) = runs
        .into_iter()
        .max_by_key(Vec::len)
        .filter(|r| !r.is_empty())
    else {
        return false;
    };
    let (k, needle) = run
        .iter()
        .enumerate()
        .max_by_key(|(_, w)| w.chars().count())
        .expect("a run holds a word");
    let mut forms = vec![needle.clone(), capitalised(needle), needle.to_uppercase()];
    forms.dedup();
    forms.iter().any(|form| {
        text.match_indices(form.as_str())
            .any(|(at, w)| stands(text, at, at + w.len(), &run, k))
    })
}

/// Whether the word at bytes `[from, to)` of `text` is `run[k]` with the rest
/// of `run` on either side of it.
fn stands(text: &str, from: usize, to: usize, run: &[String], k: usize) -> bool {
    let (before, after) = (&text[..from], &text[to..]);
    if before
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        || after.chars().next().is_some_and(char::is_alphanumeric)
    {
        return false;
    }
    let mut back = before.chars().rev();
    let behind = run[..k].iter().rev().all(|w| {
        next_word(&mut back)
            .is_some_and(|got| got.chars().rev().flat_map(char::to_lowercase).eq(w.chars()))
    });
    let mut ahead = after.chars();
    behind
        && run[k + 1..].iter().all(|w| {
            next_word(&mut ahead)
                .is_some_and(|got| got.chars().flat_map(char::to_lowercase).eq(w.chars()))
        })
}

/// The next word `chars` reach past the marks and spaces before it, in the
/// order `chars` travels.
fn next_word(chars: &mut impl Iterator<Item = char>) -> Option<String> {
    let mut word = String::new();
    for c in chars.by_ref() {
        if c.is_alphanumeric() {
            word.push(c);
        } else if !word.is_empty() {
            return Some(word);
        }
    }
    (!word.is_empty()).then_some(word)
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::holds_words;

    const MILL: &str = "They grow richer, as it were, in their sleep, without working.";

    #[test]
    fn a_quotation_standing_word_for_word_is_held_whatever_its_marks() {
        assert!(holds_words(MILL, "in their sleep."));
        assert!(holds_words(MILL, "In their sleep"));
        assert!(holds_words(MILL, "grow richer as it were in their sleep"));
        assert!(
            holds_words(MILL, "they grow richer, … without working"),
            "the longest run between gaps"
        );
    }

    #[test]
    fn other_words_or_a_word_inside_another_are_not() {
        assert!(!holds_words(MILL, "in their population."));
        assert!(!holds_words(MILL, "their sleeping"));
        assert!(!holds_words(MILL, "as it were in sleep"));
        assert!(!holds_words(MILL, "..."));
    }
}
