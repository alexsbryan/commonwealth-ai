// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which alignments are kept: the texts' words, how rare each quotation word
//! is among them, and whether an alignment's matched words are specific to
//! its place (OICP v0.6 §2.18, PAL1).

use std::collections::HashMap;

use crate::align::{fold, Quote, Source};
use crate::config::AlignConfig;
use crate::tokens::Item;

/// One text's words: their token indices, and each case-folded.
pub(crate) type SourceWords = (Vec<usize>, Vec<Vec<char>>);

/// Every text's words, case-folded once, for seeding and for [`rarity`].
pub(crate) fn folded_words(sources: &[Source]) -> Vec<SourceWords> {
    sources
        .iter()
        .map(|src| {
            let words: Vec<usize> = (0..src.tokens.len())
                .filter(|&t| src.tokens[t].word)
                .collect();
            let folded = words
                .iter()
                .map(|&t| fold(src.chars(&src.tokens[t])))
                .collect();
            (words, folded)
        })
        .collect()
}

/// Each quotation word item's weight: how rare the word is in the texts
/// read, `ln(1 + words / (1 + occurrences))`, case-folded. "there", "is" and
/// "no" weigh little against "alternative", so three common words matched
/// are not most of "there is no alternative" (PAL1).
pub(crate) fn rarity(q: &Quote, source_words: &[SourceWords]) -> HashMap<usize, f32> {
    let quote_words: Vec<(usize, Vec<char>)> = q
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            Item::Tok(t) if t.word => Some((i, fold(q.chars(t)))),
            _ => None,
        })
        .collect();
    let mut counts: HashMap<&[char], usize> =
        quote_words.iter().map(|(_, w)| (w.as_slice(), 0)).collect();
    let mut total = 0usize;
    for (_, folded) in source_words {
        total += folded.len();
        for w in folded {
            if let Some(c) = counts.get_mut(w.as_slice()) {
                *c += 1;
            }
        }
    }
    quote_words
        .iter()
        .map(|(i, w)| {
            let c = counts[w.as_slice()];
            (*i, (1.0 + total as f32 / (1 + c) as f32).ln())
        })
        .collect()
}

/// Whether the words an alignment matched say it is this place, not any
/// place with the quotation's common words: their rarity-weighted share of
/// the quotation's words reaches [`AlignConfig::specific_floor`], or they
/// hold a run of [`AlignConfig::specific_run`] words in a row. "there is no"
/// matched of "there is no alternative" is neither; "To be, or not to be,
/// that is the" matched of Hamlet with "problem" for "question" is the run.
pub(crate) fn specific(
    q: &Quote,
    matched: &[usize],
    weights: &HashMap<usize, f32>,
    cfg: &AlignConfig,
) -> bool {
    let all: f32 = weights.values().sum();
    let got: f32 = matched.iter().filter_map(|i| weights.get(i)).sum();
    if all > 0.0 && got / all >= cfg.specific_floor {
        return true;
    }
    // A run of matched words: a mark between two words keeps it, a gap or an
    // unmatched word ends it.
    let (mut run, mut best) = (0usize, 0usize);
    for (i, item) in q.items.iter().enumerate() {
        match item {
            Item::Tok(t) if t.word => {
                run = if matched.contains(&i) { run + 1 } else { 0 };
                best = best.max(run);
            }
            Item::Tok(_) => {}
            Item::Gap { .. } => run = 0,
        }
    }
    best >= cfg.specific_run
}
