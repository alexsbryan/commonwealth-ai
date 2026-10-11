//! Locating a quote inside the retrieved passages, character by character.
//!
//! Split out of `citation.rs` (987 lines, ARCH §3.1's approach band). The
//! parent owns the POLICY — prompt, extraction, the verify/abstain decision;
//! this owns the one mechanical question underneath it: given a quote the
//! model emitted and the chunks it was given, where does that text actually
//! sit, and what are the source's own characters for that span?
//!
//! It is a closed unit. `ci_ws_match_at`, `continuations_after`,
//! `whitespace_tolerant_match_at` and `longest_clean_run` have no caller
//! outside this file and stay private to it; only what the parent and its
//! tests reach is `pub(super)`. Where a quote stands is the one aligner's
//! answer (`quote_align`, ADDRESSED_TEXT §5.3), asked over case-folded copies.

use super::{MAX_TAIL_RUN, MIN_VERBATIM_RUN};

/// The grounded completion of a mid-token generation stop, if one is warranted.
/// Tries `sources` in order (first source containing the text decides — pass the
/// verified quote before the chunks so declared provenance wins). Within that
/// source, every occurrence must agree:
/// - any occurrence followed by a token boundary → the text IS a complete token
///   there → `None` (nothing to fix);
/// - all occurrences followed by the SAME alphanumeric run (≤ `MAX_TAIL_RUN`) →
///   `Some(text + run)`;
/// - disagreeing or oversized continuations → `None` (ambiguous — don't guess).
/// Whitespace-run tolerant (a quote's single spaces match a chunk's newlines),
/// case-exact (the text is a copy; a case drift means it is not this span).
pub(super) fn extend_mid_token_copy<'a>(
    text: &str,
    sources: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let needle = text.trim_end();
    if needle.is_empty() {
        return None;
    }
    for src in sources {
        let conts = continuations_after(src, needle);
        if conts.is_empty() {
            continue; // not in this source — try the next
        }
        if conts.iter().any(|c| c.is_empty()) {
            return None; // complete token somewhere — no truncation to repair
        }
        let first = &conts[0];
        if conts.iter().all(|c| c == first) && first.chars().count() <= MAX_TAIL_RUN {
            return Some(format!("{needle}{first}"));
        }
        return None; // ambiguous continuations in the provenance source
    }
    None
}

/// The QUOTE-cased span the answer is a case-garbled copy of, if any: the
/// answer occurs in the quote under case-insensitive (and whitespace-tolerant)
/// matching, and the quote's exact-case span differs. Returns `None` when the
/// answer isn't a quote span or is already exact. Restoring the quote's casing
/// can only make the answer MORE faithful to the verified source text — it
/// also repairs de-capitalized proper nouns, not just formula variables.
pub(super) fn snap_answer_case_to_quote(answer: &str, quote: &str) -> Option<String> {
    let q: Vec<char> = quote.chars().collect();
    let n: Vec<char> = answer.trim().chars().collect();
    if n.is_empty() {
        return None;
    }
    for start in 0..q.len() {
        if let Some(end) = ci_ws_match_at(&q, start, &n) {
            let span: String = q[start..end].iter().collect();
            return (span != answer.trim()).then_some(span);
        }
    }
    None
}

/// `whitespace_tolerant_match_at`, case-insensitively.
fn ci_ws_match_at(h: &[char], start: usize, n: &[char]) -> Option<usize> {
    let mut i = start;
    let mut j = 0usize;
    let eq = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    while j < n.len() {
        if n[j].is_whitespace() {
            if i >= h.len() || !h[i].is_whitespace() {
                return None;
            }
            while i < h.len() && h[i].is_whitespace() {
                i += 1;
            }
            while j < n.len() && n[j].is_whitespace() {
                j += 1;
            }
        } else {
            if i >= h.len() || !eq(h[i], n[j]) {
                return None;
            }
            i += 1;
            j += 1;
        }
    }
    Some(i)
}

/// The alphanumeric run immediately following each whitespace-tolerant
/// occurrence of `needle` in `hay` (empty string = the occurrence ends at a
/// token boundary). Runs are truncated at `MAX_TAIL_RUN + 1` chars so an
/// oversized continuation is detectable without unbounded collection.
fn continuations_after(hay: &str, needle: &str) -> Vec<String> {
    let h: Vec<char> = hay.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < h.len() {
        if let Some(end) = whitespace_tolerant_match_at(&h, i, &n) {
            let mut run = String::new();
            let mut k = end;
            while k < h.len() && h[k].is_alphanumeric() && run.chars().count() <= MAX_TAIL_RUN {
                run.push(h[k]);
                k += 1;
            }
            out.push(run);
        }
        i += 1;
    }
    out
}

/// Match `needle` at `h[start..]` treating any whitespace run as equivalent to
/// any other. Returns the hay index one past the match.
fn whitespace_tolerant_match_at(h: &[char], start: usize, n: &[char]) -> Option<usize> {
    let mut i = start;
    let mut j = 0usize;
    while j < n.len() {
        if n[j].is_whitespace() {
            if i >= h.len() || !h[i].is_whitespace() {
                return None;
            }
            while i < h.len() && h[i].is_whitespace() {
                i += 1;
            }
            while j < n.len() && n[j].is_whitespace() {
                j += 1;
            }
        } else {
            if i >= h.len() || h[i] != n[j] {
                return None;
            }
            i += 1;
            j += 1;
        }
    }
    Some(i)
}

pub(super) fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where a verified quote was found — and, when the passage can be quoted back
/// verbatim, the source's OWN text for that span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QuoteMatch {
    /// The WHOLE quote sits in ONE passage as one contiguous run. `verbatim` is
    /// the source's own characters for that run, and it is what the release
    /// prints — see `verify_pair`. Because it is a substring of a chunk, the
    /// downstream strict re-check (`quote_verification::verify_quotes`, which
    /// demands one contiguous source substring) cannot demote it. This is the
    /// ONLY match that may carry a section locator.
    Exact { chunk: usize, verbatim: String },
    /// Only a run of ≥`MIN_VERBATIM_RUN` consecutive words matched — the model's
    /// span diverges from the source somewhere, so it is NOT a contiguous source
    /// substring even though it is grounded. Carries no locator: the strict
    /// re-check will rewrite this span to `[unverified excerpt: …]`, and a
    /// heading on an unverified excerpt claims more than the text it labels.
    Partial { chunk: usize },
    /// Verbatim only across the joined passages, so no single chunk owns it.
    /// Still grounded (the text is corpus text either way); simply not
    /// attributable to one source, and reported as such rather than being
    /// assigned to whichever chunk happens to be first.
    AcrossChunks,
}

/// Is `quote` a verbatim span of the passages, and if so, where? The whole
/// quote in one passage, or a run of ≥`MIN_VERBATIM_RUN` consecutive words of
/// it (the model trimmed the edges). A paraphrase or a fabricated "quote"
/// matches neither.
///
/// Asked of the one aligner (ADDRESSED_TEXT §5.3) over case-folded copies of
/// both sides (`quote_verification::fold_case`): this path has always matched
/// case-insensitively, and case is the caller's call. Each pass keeps the
/// order and the question it had before the convergence:
/// 1. **Exact** — the whole quote in one passage, in exact mode
///    (`quote_verification::align_exact`), handed back as the passage's OWN
///    characters for that range;
/// 2. **Partial** — the first passage (then position) where the aligner
///    matched `MIN_VERBATIM_RUN` consecutive words of the quote with no
///    difference among them ([`longest_clean_run`]);
/// 3. **AcrossChunks** — either question of the passages joined, so a quote
///    that straddles a chunk boundary still grounds, unattributed.
///
/// Pass 1 grounds nothing the other two would not: a whole quote verbatim in
/// one passage is verbatim in the passages joined (pass 3), and when it has
/// six or more words it is a run of six (pass 2). The grounding decision is
/// the run test, asked of the aligner's differences instead of a substring
/// of a fold.
pub(super) fn locate_quote_in_chunks(quote: &str, chunks: &[String]) -> Option<QuoteMatch> {
    use crate::quote_verification::{align_exact, fold_case};
    let words = normalize(quote)
        .split(' ')
        .filter(|w| !w.is_empty())
        .count();
    if words < 3 {
        return None; // too short to be a genuine supporting sentence
    }
    let quote = quote.trim();
    let folded_quote = fold_case(quote);
    let folded: Vec<String> = chunks.iter().map(|c| fold_case(c)).collect();
    let texts: Vec<&str> = folded.iter().map(String::as_str).collect();
    // Pass 1 — the whole quote in one passage, as the SOURCE's own characters
    // (the folded copies keep every code point where it was).
    if let Some((chunk, range)) = align_exact(&folded_quote, &texts) {
        if let Some(verbatim) = quote_align::code_point_slice(&chunks[chunk], range) {
            return Some(QuoteMatch::Exact {
                chunk,
                verbatim: verbatim.to_string(),
            });
        }
    }
    let cfg = run_config()?;
    let has_run = |a: &quote_align::QuoteAlignment| {
        longest_clean_run(&folded_quote, &a.edits) >= MIN_VERBATIM_RUN
    };
    // Pass 2 — a run inside one passage, first passage then first position.
    if words >= MIN_VERBATIM_RUN {
        let mut found = quote_align::align(&folded_quote, &texts, cfg).alignments;
        found.sort_by_key(|a| (a.text, a.source.start));
        if let Some(a) = found.iter().find(|a| has_run(a)) {
            return Some(QuoteMatch::Partial { chunk: a.text });
        }
    }
    // Pass 3 — either question of the joined passages.
    let joined = fold_case(&chunks.join(" "));
    let across = align_exact(&folded_quote, &[joined.as_str()]).is_some()
        || (words >= MIN_VERBATIM_RUN
            && quote_align::align(&folded_quote, &[joined.as_str()], cfg)
                .alignments
                .iter()
                .any(has_run));
    across.then_some(QuoteMatch::AcrossChunks)
}

/// The aligner's knobs with the coverage floor at its least: a run covers as
/// little of the quote as six of its words, and this path decides on the run,
/// not on coverage. `None` as the shipped knobs are (`align_config`).
fn run_config() -> Option<&'static quote_align::AlignConfig> {
    static CFG: std::sync::OnceLock<Option<quote_align::AlignConfig>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        crate::quote_verification::align_config().map(|c| quote_align::AlignConfig {
            coverage_floor: f32::MIN_POSITIVE,
            ..c.clone()
        })
    })
    .as_ref()
}

/// The most consecutive whitespace-separated words of `quote` the aligner
/// matched with no difference touching them or the gaps between them. A word
/// is clean when no difference overlaps it; two clean words are consecutive
/// when no difference lies between them, an omission (an empty range in the
/// quote) included, since then the source has words the quote skipped.
fn longest_clean_run(quote: &str, edits: &[quote_align::QuoteEdit]) -> usize {
    // Each word's code-point range in `quote`.
    let mut words: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    let mut at = 0usize;
    for c in quote.chars() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(s)) => {
                words.push(s..at);
                start = None;
            }
            _ => {}
        }
        at += 1;
    }
    if let Some(s) = start {
        words.push(s..at);
    }
    let touches = |span: std::ops::Range<usize>| {
        edits.iter().any(|e| {
            if e.quote.is_empty() {
                span.start < e.quote.start && e.quote.start < span.end
            } else {
                e.quote.start < span.end && span.start < e.quote.end
            }
        })
    };
    let (mut best, mut run) = (0usize, 0usize);
    for (k, w) in words.iter().enumerate() {
        if touches(w.clone()) {
            run = 0;
            continue;
        }
        // The gap from the previous word, both ends included, so an
        // omission at either edge of it breaks the run.
        let joined = k > 0 && run > 0 && !touches(words[k - 1].end - 1..w.start + 1);
        run = if joined { run + 1 } else { 1 };
        best = best.max(run);
    }
    best
}

#[cfg(test)]
mod tests {
    use quote_align::{QuoteEdit, QuoteEditKind};

    use super::{locate_quote_in_chunks, longest_clean_run, QuoteMatch};

    /// The citation path folds what the quote guard folds (`norm_v0`, through
    /// the one aligner): a quote copied with a straight apostrophe grounds in
    /// a passage that has the typographic one, and the release prints the
    /// passage's own characters. Red against the old lowercase-and-whitespace
    /// substring test, which found no run of six words without the apostrophe.
    #[test]
    fn a_straight_apostrophe_quote_grounds_in_a_typographic_passage() {
        let chunks = vec!["It was Mr Verloc\u{2019}s shop then, small and dim.".to_string()];
        assert_eq!(
            locate_quote_in_chunks("It was Mr Verloc's shop then", &chunks),
            Some(QuoteMatch::Exact {
                chunk: 0,
                verbatim: "It was Mr Verloc\u{2019}s shop then".to_string(),
            })
        );
    }

    /// A quote that silently skips a source word between two runs of four is
    /// no run of six: the passage does not say those six words in a row.
    #[test]
    fn a_quote_that_skips_a_source_word_is_not_a_run() {
        let chunks =
            vec!["Tabb greased the eastern pawls before the gulls woke properly.".to_string()];
        assert_eq!(
            locate_quote_in_chunks("greased the eastern pawls the gulls woke properly", &chunks),
            None
        );
    }

    fn edit(kind: QuoteEditKind, quote: std::ops::Range<usize>) -> QuoteEdit {
        QuoteEdit {
            kind,
            quote,
            source: 0..0,
        }
    }

    /// A difference on a word ends the run at it, and an omission between
    /// two words ends it between them: the source has words the quote
    /// skipped there.
    #[test]
    fn a_run_stops_at_a_difference_and_at_an_omission() {
        let q = "one two three four five six seven eight";
        assert_eq!(longest_clean_run(q, &[]), 8);
        // "four" substituted: runs of three and four.
        assert_eq!(
            longest_clean_run(q, &[edit(QuoteEditKind::Substituted, 14..18)]),
            4
        );
        // Words omitted between "five" and "six" (the point after "five").
        assert_eq!(
            longest_clean_run(q, &[edit(QuoteEditKind::Omitted, 23..23)]),
            5
        );
        // An omission at the quote's very start breaks nothing.
        assert_eq!(
            longest_clean_run(q, &[edit(QuoteEditKind::Omitted, 0..0)]),
            8
        );
    }
}
