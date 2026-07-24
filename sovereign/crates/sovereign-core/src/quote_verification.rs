// SPDX-License-Identifier: AGPL-3.0-or-later
//! Post-generation guardrail: verify every quoted passage in the
//! model's final answer is verbatim-present in the document, and
//! demote any that aren't.
//!
//! # Why
//!
//! The book-report bench (Run 7, 2026-05-22) surfaced a failure
//! pattern worse than a bad answer: the model produced a fluent,
//! well-cited essay containing several "composite quotes" — real
//! Conrad fragments joined with `...` ellipsis into passages that
//! don't appear continuously in any chunk. The bench's hallucination
//! detector flagged them; an end user reading the answer cannot tell
//! a composite quote from a continuous one, and trust evaporates the
//! first time they spot one.
//!
//! Production-grade guardrail: before returning an answer to the user,
//! scan every quoted span ≥ N chars, verify substring-presence against
//! the document's chunks, and demote unverified spans from `"..."` to
//! `[unverified excerpt: ...]`. The prose around the quote survives;
//! the deceptive verbatim framing does not.
//!
//! # Scope
//!
//! This module is generic over "what counts as the document." Callers
//! pass a slice of source strings (typically all chunks for the
//! attached asset). Composite quotes naturally fail verification
//! because their joined form isn't continuous anywhere — that's the
//! intended outcome.

/// Default minimum span length to verify, in characters. Spans shorter
/// than this are presumed to be technical terms or short references
/// (e.g. `"frail"`) where verbatim-presence is overwhelmingly likely
/// and the cost of false positives outweighs the value of catching
/// the rare actual fabrication. The bench's hallucination detector
/// uses 30 chars; 40 here is slightly looser so we don't flag
/// legitimate short citations that the bench would.
pub const DEFAULT_MIN_QUOTE_CHARS: usize = 40;

/// Outcome of a verification pass.
#[derive(Debug, Clone, Default)]
pub struct VerificationResult {
    /// The rewritten answer text with unverified quotes demoted.
    pub rewritten: String,
    /// Number of quoted spans that passed verification (kept as-is).
    pub verified_count: usize,
    /// Number of quoted spans that failed verification (demoted).
    pub demoted_count: usize,
}

/// Scan `answer` for quoted spans of `min_chars` or more, verify each
/// against the union of `source_chunks` + `extra_verbatim_spans`, and
/// rewrite unverified spans to `[unverified excerpt: ...]`. Returns
/// the rewritten answer plus counts.
///
/// `extra_verbatim_spans` is for spans the runtime knows are verbatim
/// by construction (e.g. RAPTOR node `quote_spans`). They're checked
/// in addition to `source_chunks` so a verified verbatim span that
/// happens to span a chunk boundary still passes.
///
/// Normalisation: both the quote and the source are folded before
/// substring comparison — whitespace runs collapse to a single space,
/// typographic characters fold to ASCII (curly quotes/apostrophes,
/// em/en dashes, `…`), and markdown emphasis markers (`*`, `` ` ``,
/// `_`) are stripped. This handles markdown line breaks vs source
/// line wraps, models quoting `’`-apostrophe source text with `'`,
/// bold-face inside quotes, and Gutenberg `_italics_` markers —
/// each observed as a false demotion on the 2026-07-23 eye test.
/// Leading/trailing ellipses on the quote are trimmed (edge elision
/// is honest quoting); interior ellipses still fail verification —
/// the composite-quote policy is unchanged, because a spliced quote
/// is non-contiguous in the source under any normalisation.
///
/// Quote detection: handles straight double quotes (`"..."`) and
/// curly/smart double quotes (`"..."`). Single-quote spans are not
/// verified — they're commonly used for dialogue *within* quoted text
/// or for technical terms, and aggressive single-quote checking would
/// flag legitimate uses.
pub fn verify_quotes(
    answer: &str,
    source_chunks: &[String],
    extra_verbatim_spans: &[String],
    min_chars: usize,
) -> VerificationResult {
    let mut result = VerificationResult::default();

    // Pre-normalise the sources so we don't redo it per-quote.
    let normalised_sources: Vec<String> = source_chunks
        .iter()
        .chain(extra_verbatim_spans.iter())
        .map(|s| normalise_for_match(s))
        .collect();

    let mut out = String::with_capacity(answer.len());
    let chars: Vec<char> = answer.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Detect the start of a quoted span. Accept straight and curly
        // double quotes as openers.
        if is_double_quote_open(c) {
            // Find the matching close. Same character class (any
            // double-quote-like) closes the span; we don't enforce
            // matching opener/closer pairs because the model often
            // mixes them.
            if let Some(close) = find_double_quote_close(&chars, i + 1) {
                let inner: String = chars[i + 1..close].iter().collect();
                if inner.chars().count() >= min_chars {
                    let normalised_quote = trim_edge_ellipses(&normalise_for_match(&inner));
                    let verified = !normalised_quote.is_empty()
                        && normalised_sources
                            .iter()
                            .any(|src| src.contains(&normalised_quote));
                    if verified {
                        // Keep as-is: re-emit `"inner"`.
                        out.push(c);
                        out.push_str(&inner);
                        out.push(chars[close]);
                        result.verified_count += 1;
                    } else {
                        // Demote. Strip ellipsis-bridged composites
                        // by replacing the quote marks; keep the
                        // inner text so the surrounding prose still
                        // reads, but signal the user that the framing
                        // was promoted-from-paraphrase, not verbatim.
                        out.push_str("[unverified excerpt: ");
                        out.push_str(&inner);
                        out.push(']');
                        result.demoted_count += 1;
                    }
                    i = close + 1;
                    continue;
                }
                // Short quote — pass through unchanged.
                out.push(c);
                for &qc in &chars[i + 1..=close] {
                    out.push(qc);
                }
                i = close + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }

    result.rewritten = out;
    result
}

/// Convenience wrapper for corpus-grounded synthesis paths (KnowledgeQuery
/// streaming + non-streaming, post-stream refinement) where the source
/// evidence is already assembled as a single formatted string — the exact
/// chunk text the model was shown — rather than a per-chunk slice.
///
/// Guard: when `evidence` is empty (the parametric / retrieval-miss path,
/// where `doc_context` is `""`), the answer is returned **unchanged**. We
/// have no source to verify against, so we must not demote — a quote can
/// only be called unverified when there was something to check it against.
/// This mirrors the attached-doc guardrail's graceful-degradation contract:
/// an empty verification surface leaves the answer untouched.
///
/// A genuine verbatim quote from any retrieved chunk is a whitespace-folded
/// substring of the concatenated evidence and passes; a composite or
/// fabricated quote is not contiguous anywhere in it and is demoted.
pub fn verify_answer_against_evidence(answer: &str, evidence: &str) -> VerificationResult {
    if evidence.trim().is_empty() {
        return VerificationResult {
            rewritten: answer.to_string(),
            verified_count: 0,
            demoted_count: 0,
        };
    }
    let sources = [evidence.to_string()];
    verify_quotes(answer, &sources, &[], DEFAULT_MIN_QUOTE_CHARS)
}

/// Fold text for substring comparison. Applied symmetrically to
/// quotes and sources:
/// - whitespace runs collapse to a single space;
/// - typographic characters fold to ASCII: `‘ ’ ʼ` → `'`, `“ ”` → `"`,
///   `– —` → `-`, `…` → `...` — models routinely restyle these when
///   quoting, and Gutenberg sources use the typographic forms;
/// - markdown emphasis markers `*`, `` ` ``, `_` are dropped: models
///   bold spans inside quotes, and Gutenberg renders italics as
///   `_underscores_`. Dropping them on BOTH sides keeps the
///   comparison symmetric, so a source's literal `_` can still match
///   a quote that omitted it.
///
/// Deliberately NOT folded: letter case (a case-mismatched "quote" is
/// not verbatim) and interior punctuation (a spliced composite must
/// keep failing).
fn normalise_for_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        let mapped: Option<char> = match c {
            '\u{2018}' | '\u{2019}' | '\u{02BC}' => Some('\''),
            '\u{201C}' | '\u{201D}' => Some('"'),
            '\u{2013}' | '\u{2014}' => Some('-'),
            '*' | '`' | '_' => None,
            '\u{2026}' => {
                out.push_str("...");
                prev_was_space = false;
                continue;
            }
            c => Some(c),
        };
        match mapped {
            Some(c) if c.is_whitespace() => {
                if !prev_was_space {
                    out.push(' ');
                    prev_was_space = true;
                }
            }
            Some(c) => {
                out.push(c);
                prev_was_space = false;
            }
            None => {}
        }
    }
    out.trim().to_string()
}

/// Trim leading/trailing ellipsis runs (plus surrounding whitespace)
/// from a normalised quote. `"...the spectre took its crawl..."` is
/// honest edge-elision, not a composite — the elided part is OUTSIDE
/// the quoted span. Interior ellipses are untouched, so spliced
/// composites keep failing verification.
fn trim_edge_ellipses(s: &str) -> String {
    s.trim_matches(|c: char| c == '.' || c.is_whitespace())
        .trim()
        .to_string()
}

/// `true` if `c` is a double-quote character that opens a span we
/// should verify.
fn is_double_quote_open(c: char) -> bool {
    c == '"' || c == '\u{201C}' || c == '\u{201D}'
}

/// Find the next character index in `chars[from..]` that closes a
/// double-quote span. Mirrors `is_double_quote_open` for symmetric
/// detection — the model often mixes `"..."` and `"..."`.
fn find_double_quote_close(chars: &[char], from: usize) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == '"' || c == '\u{201C}' || c == '\u{201D}')
        .map(|p| p + from)
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_evidence_leaves_answer_unchanged() {
        // Parametric / retrieval-miss path: doc_context is empty. Even a
        // long quoted span must NOT be demoted — there is no source to
        // verify against, so demotion would be a false accusation.
        let answer = r#"Kant argues that "the categorical imperative binds all rational agents unconditionally and without exception.""#;
        let r = verify_answer_against_evidence(answer, "");
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 0);
        assert_eq!(r.rewritten, answer);
        // Whitespace-only evidence is treated the same as empty.
        let r2 = verify_answer_against_evidence(answer, "   \n  ");
        assert_eq!(r2.rewritten, answer);
        assert_eq!(r2.demoted_count, 0);
    }

    #[test]
    fn fabricated_quote_against_evidence_is_demoted() {
        // SEP-shaped evidence: a real passage the model was shown. The
        // answer fabricates a verbatim-looking quote that never appears.
        let evidence =
            "Compatibilism is the thesis that free will is compatible with determinism. \
             Classical compatibilists analyse the freedom to do otherwise as a hypothetical: \
             an agent could have done otherwise if she had chosen to.";
        let answer = r#"On this view, Frankfurt holds that "moral responsibility floats entirely free of any ability to do otherwise whatsoever.""#;
        let r = verify_answer_against_evidence(answer, evidence);
        assert_eq!(r.demoted_count, 1);
        assert!(r.rewritten.contains("[unverified excerpt:"));
    }

    #[test]
    fn verbatim_quote_against_evidence_passes() {
        let evidence =
            "Compatibilism is the thesis that free will is compatible with determinism. \
             Classical compatibilists analyse the freedom to do otherwise as a hypothetical.";
        let answer = r#"The entry defines it directly: "free will is compatible with determinism" is the core claim."#;
        let r = verify_answer_against_evidence(answer, evidence);
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 1);
        assert_eq!(r.rewritten, answer);
    }

    #[test]
    fn verified_quote_passes_through_unchanged() {
        let source = "Stevie sat at a deal table, drawing circles, circles, circles; innumerable circles, concentric, eccentric.".to_string();
        let answer = r#"The narrator says "drawing circles, circles, circles; innumerable circles, concentric, eccentric" in chapter one."#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 1);
        assert!(r.rewritten.contains(
            r#""drawing circles, circles, circles; innumerable circles, concentric, eccentric""#
        ));
    }

    #[test]
    fn unverified_quote_is_demoted() {
        let source = "He walked through the empty streets.".to_string();
        let answer = r#"As Conrad writes, "the professor seized the policeman by the throat with great violence and intent.""#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.demoted_count, 1);
        assert_eq!(r.verified_count, 0);
        assert!(r.rewritten.contains("[unverified excerpt:"));
        assert!(!r.rewritten.contains(r#""the professor seized"#));
    }

    #[test]
    fn composite_quote_with_ellipsis_fails_verification() {
        // The two fragments are real; the composite isn't continuous
        // anywhere in the source — exactly the failure mode this
        // module is built to catch.
        let chunk_a = "He smiled no longer his enigmatic and mocking smile.".to_string();
        let chunk_b = "It was a sad-faced, miserable little man who emerged.".to_string();
        let answer = r#"Conrad writes, "He smiled no longer his enigmatic mocking smile... It was a sad-faced, miserable little man who emerged.""#;
        let r = verify_quotes(answer, &[chunk_a, chunk_b], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.demoted_count, 1, "composite quotes must be demoted");
        assert!(r.rewritten.contains("[unverified excerpt:"));
    }

    #[test]
    fn short_quotes_below_min_chars_pass_through() {
        let source = "The professor walks alone.".to_string();
        let answer = r#"The "professor" is the focus."#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        // Quote is shorter than DEFAULT_MIN_QUOTE_CHARS — neither
        // verified nor demoted; just passed through.
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 0);
        assert!(r.rewritten.contains(r#""professor""#));
    }

    #[test]
    fn whitespace_normalised_quote_verifies() {
        // Source has a hard line break inside the quoted phrase.
        let source = "He found himself walking\nthrough the empty streets at dawn.".to_string();
        let answer =
            r#"Conrad says he was "walking through the empty streets at dawn" — a key moment."#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1);
        assert_eq!(r.demoted_count, 0);
    }

    #[test]
    fn curly_quotes_are_recognised() {
        // Sources use straight quotes; answer uses smart curly quotes.
        let source = "She found the wedding ring hidden in her pocket.".to_string();
        let answer = "He recalls Winnie\u{201C}found the wedding ring hidden in her pocket\u{201D} in chapter twelve.";
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1);
    }

    #[test]
    fn extra_verbatim_spans_supplement_chunks() {
        // The full chunk doesn't contain the quote, but a RAPTOR
        // quote_span does — verification should still pass.
        let chunk = "Something else entirely from the document.".to_string();
        let raptor_span = "the haunting fear of his sinister loneliness".to_string();
        let answer = r#"The professor is described with "the haunting fear of his sinister loneliness" in the encounter."#;
        let r = verify_quotes(answer, &[chunk], &[raptor_span], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1);
        assert_eq!(r.demoted_count, 0);
    }

    #[test]
    fn multiple_quotes_in_one_answer_independent_outcomes() {
        let source =
            "Stevie drew his circles, circles, circles all afternoon long in silence.".to_string();
        let answer = r#"The narrator says "Stevie drew his circles, circles, circles all afternoon" but also "the moon rose over the empty hills above the silent town" later."#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1);
        assert_eq!(r.demoted_count, 1);
        assert!(r
            .rewritten
            .contains(r#""Stevie drew his circles, circles, circles all afternoon""#));
        assert!(r.rewritten.contains("[unverified excerpt: the moon"));
    }

    #[test]
    fn normalise_for_match_folds() {
        assert_eq!(normalise_for_match("a  b\t\nc"), "a b c");
        assert_eq!(
            normalise_for_match("  leading and trailing  "),
            "leading and trailing"
        );
        assert_eq!(normalise_for_match(""), "");
        assert_eq!(
            normalise_for_match("Mrs Verloc\u{2019}s \u{201C}gaze\u{201D} \u{2014} steady"),
            "Mrs Verloc's \"gaze\" - steady"
        );
        assert_eq!(
            normalise_for_match("**bold** and _italic_"),
            "bold and italic"
        );
        assert_eq!(normalise_for_match("wait\u{2026} what"), "wait... what");
    }

    #[test]
    fn curly_apostrophe_in_source_matches_straight_in_quote() {
        // Gutenberg text uses U+2019; models quote with '. Observed
        // false demotion class #1 on the 2026-07-23 eye test.
        let source =
            "Winnie\u{2019}s philosophy consisted in not taking notice of the inside of facts."
                .to_string();
        let answer = r#"The narrator notes that "Winnie's philosophy consisted in not taking notice of the inside of facts.""#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 1);
    }

    #[test]
    fn markdown_bold_inside_quote_matches_plain_source() {
        let source =
            "where that spectre took its constitutional crawl every fine morning.".to_string();
        let answer = r#"Conrad writes "that spectre took its **constitutional crawl** every fine morning" of Yundt."#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.demoted_count, 0);
        assert_eq!(r.verified_count, 1);
    }

    #[test]
    fn gutenberg_underscore_italics_match_unmarked_quote() {
        let source = "He read the _Morning Post_ with an air of complete detachment.".to_string();
        let answer =
            r#"He is seen reading: "He read the Morning Post with an air of complete detachment.""#;
        let r = verify_quotes(answer, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1);
        assert_eq!(r.demoted_count, 0);
    }

    #[test]
    fn edge_ellipses_trimmed_interior_composites_still_fail() {
        let source = "Jolly lucky for Yundt that she had persisted in coming up time after time."
            .to_string();
        // Edge elision: honest quoting, must verify.
        let edge = r#"As the text says, "...she had persisted in coming up time after time..." throughout."#;
        let r = verify_quotes(edge, &[source.clone()], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r.verified_count, 1, "edge ellipses are not composites");
        assert_eq!(r.demoted_count, 0);
        // Interior splice: still a composite, still demoted.
        let spliced = r#"As the text says, "Jolly lucky for Yundt... coming up time after time again and again.""#;
        let r2 = verify_quotes(spliced, &[source], &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(r2.demoted_count, 1, "interior splices must keep failing");
    }
}
