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
/// Normalisation: both the quote and the source are whitespace-folded
/// (runs of whitespace collapsed to a single space) before substring
/// comparison. This handles markdown line breaks vs source line wraps
/// without false-flagging legitimate quotes.
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
        .map(|s| normalise_whitespace(s))
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
                    let normalised_quote = normalise_whitespace(&inner);
                    let verified = normalised_sources
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

/// Collapse runs of whitespace to a single space. Both quotes and
/// sources are run through this before substring comparison so line
/// breaks in markdown vs the source don't cause false negatives.
fn normalise_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(c);
            prev_was_space = false;
        }
    }
    out.trim().to_string()
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
    fn normalise_whitespace_collapses_runs() {
        assert_eq!(normalise_whitespace("a  b\t\nc"), "a b c");
        assert_eq!(
            normalise_whitespace("  leading and trailing  "),
            "leading and trailing"
        );
        assert_eq!(normalise_whitespace(""), "");
    }
}
