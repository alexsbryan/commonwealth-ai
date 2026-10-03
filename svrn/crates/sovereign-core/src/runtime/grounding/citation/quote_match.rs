//! Locating a quote inside the retrieved passages, character by character.
//!
//! Split out of `citation.rs` (987 lines, ARCH §3.1's approach band). The
//! parent owns the POLICY — prompt, extraction, the verify/abstain decision;
//! this owns the one mechanical question underneath it: given a quote the
//! model emitted and the chunks it was given, where does that text actually
//! sit, and what are the source's own characters for that span?
//!
//! It is a closed unit. `ci_ws_match_at`, `continuations_after`,
//! `whitespace_tolerant_match_at` and `exact_span_in` have no caller outside
//! this file and stay private to it; only what the parent and its tests
//! reach is `pub(super)`.

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

/// Is `quote` a verbatim span of the passages, and if so, where? Full
/// normalised substring, or a run of ≥`MIN_VERBATIM_RUN` consecutive words
/// (the model trimmed the edges). A paraphrase or a fabricated "quote"
/// matches neither.
///
/// THE GROUNDING DECISION IS UNCHANGED by returning a location. The set of
/// quotes that ground is exactly what it was before locators existed: pass 2
/// below IS the original per-chunk test, and pass 3 IS the original joined-
/// haystack test. Pass 1 only *refines* a match pass 2 would have accepted
/// anyway — it never admits one pass 2 would reject, because it is gated on the
/// same `hay.contains(&q)`. Tightening to per-chunk-only would have moved a
/// fabrication guard while claiming to add a label (ARCH_PRINCIPLES §10.6 —
/// one decider; this is the same decider, saying more).
pub(super) fn locate_quote_in_chunks(quote: &str, chunks: &[String]) -> Option<QuoteMatch> {
    let q = normalize(quote);
    let words: Vec<&str> = q.split(' ').filter(|w| !w.is_empty()).collect();
    if words.len() < 3 {
        return None; // too short to be a genuine supporting sentence
    }
    let present_in = |hay: &str| -> bool {
        hay.contains(&q)
            || (words.len() >= MIN_VERBATIM_RUN
                && words
                    .windows(MIN_VERBATIM_RUN)
                    .any(|w| hay.contains(&w.join(" "))))
    };
    let normalised: Vec<String> = chunks.iter().map(|c| normalize(c)).collect();
    // Pass 1 — the whole quote in one passage, recovered as the SOURCE's own
    // characters. `hay.contains(&q)` is the cheap gate; `exact_span_in` then
    // re-finds the span in the raw chunk so we can hand back untouched source
    // text rather than the model's copy of it.
    for (i, hay) in normalised.iter().enumerate() {
        if hay.contains(&q) {
            if let Some(verbatim) = exact_span_in(&chunks[i], quote) {
                return Some(QuoteMatch::Exact { chunk: i, verbatim });
            }
        }
    }
    // Pass 2 — the original per-chunk decision, unchanged.
    for (i, hay) in normalised.iter().enumerate() {
        if present_in(hay) {
            return Some(QuoteMatch::Partial { chunk: i });
        }
    }
    // Pass 3 — the original joined-haystack decision, unchanged.
    present_in(&normalize(&chunks.join(" "))).then_some(QuoteMatch::AcrossChunks)
}

/// The source's OWN text for the span `quote` occupies in `chunk`, if the whole
/// quote sits there as one contiguous run (case-insensitively, any whitespace
/// run matching any other).
///
/// Why the source's characters and not the model's: what we print is what the
/// downstream strict re-check reads back. A copy that differs from the source in
/// case alone passes the citation path's case-insensitive test and then FAILS
/// the strict re-check, which is case-sensitive — and the answer ships with a
/// confident section heading glued to an `[unverified excerpt: …]`. Returning
/// the source span makes "the released quote is verbatim" structural rather than
/// hoped-for (ARCH_PRINCIPLES §7).
fn exact_span_in(chunk: &str, quote: &str) -> Option<String> {
    let n: Vec<char> = quote.trim().chars().collect();
    if n.is_empty() {
        return None;
    }
    let h: Vec<char> = chunk.chars().collect();
    (0..h.len())
        .find_map(|start| ci_ws_match_at(&h, start, &n).map(|end| (start, end)))
        .map(|(start, end)| h[start..end].iter().collect())
}
