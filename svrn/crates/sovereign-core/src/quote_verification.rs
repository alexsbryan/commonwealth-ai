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
    /// The inner text of every span that PASSED verification — i.e.
    /// `verified_count` spans, verbatim as they appear in the answer
    /// (order authority-guard-at-exit, 2026-08-17). The answer-exit
    /// numeric guard reads these to exempt figures the source itself
    /// states: a numeral inside a verified verbatim quote is the
    /// filing's own sentence, not a model-originated figure (§6.2(5)).
    /// Spans below the length floor and demoted spans are never here,
    /// so quote-wrapping a fabricated figure earns no exemption.
    /// Additive field: `rewritten` and both counts are byte-identical
    /// to the pre-field behaviour on every input.
    pub verified_spans: Vec<String>,
    /// Where each span of `verified_spans` stood, in the same order: the
    /// source it was found in and the code points of it the span covers.
    /// Additive, like `verified_spans`.
    pub verified: Vec<VerifiedQuote>,
}

/// A quoted span that verified, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedQuote {
    /// The span as the answer writes it.
    pub span: String,
    /// Index of the source it stands in, in the order verified against.
    pub source: usize,
    /// The code points of that source the span covers, half-open.
    pub source_range: std::ops::Range<usize>,
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
/// Matching is the one aligner's exact mode ([`locate_verbatim`]): the
/// quote verifies when it stands verbatim in some source. Normalisation
/// (`norm_v0`): both sides are folded — NFC, whitespace runs collapse to a
/// single space, words hyphenated across a line end are joined,
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
    let sources: Vec<&str> = source_chunks
        .iter()
        .chain(extra_verbatim_spans.iter())
        .map(String::as_str)
        .collect();
    verify_against(answer, &sources, min_chars)
}

/// [`verify_quotes`] over sources the caller holds as `&str`, in the order
/// given: an earlier source wins where a quote stands in two, so a caller
/// lists first the ones it wants a verified quote addressed into.
fn verify_against(answer: &str, sources: &[&str], min_chars: usize) -> VerificationResult {
    let mut result = VerificationResult::default();
    // No surface, no verdict: with every source blank there is nothing to
    // check a quote against, so the answer stands and nothing is demoted, as
    // every caller's comment promises (`attached_doc`'s failed prefetch).
    // Until ADDRESSED_TEXT's defect 6 this demoted every checked quote.
    if sources.iter().all(|s| s.trim().is_empty()) {
        tracing::debug!(
            sources = sources.len(),
            "quote_verification: no verification surface; the answer is left unchanged"
        );
        result.rewritten = answer.to_string();
        return result;
    }

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
                    if let Some((source, source_range)) = locate_verbatim(&inner, sources) {
                        // Keep as-is: re-emit `"inner"`.
                        out.push(c);
                        out.push_str(&inner);
                        out.push(chars[close]);
                        result.verified_count += 1;
                        result.verified.push(VerifiedQuote {
                            span: inner.clone(),
                            source,
                            source_range,
                        });
                        result.verified_spans.push(inner);
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
            tracing::debug!(
                target: "quote_verification",
                offset = i,
                "quote_verification:unclosed_quote — no close before a blank line; passed through as text"
            );
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
///
/// PREFER [`verify_answer_against_turn_evidence`] wherever the caller still
/// holds the chunks. `evidence` is the *prompt rendering* of the turn's
/// sources, and that rendering truncates every chunk to
/// `runtime::text_utils::MAX_CHUNK_CHARS` (600) — see that function for the
/// measured consequence of verifying against it.
pub fn verify_answer_against_evidence(answer: &str, evidence: &str) -> VerificationResult {
    if evidence.trim().is_empty() {
        return VerificationResult {
            rewritten: answer.to_string(),
            ..VerificationResult::default()
        };
    }
    let sources = [evidence.to_string()];
    verify_quotes(answer, &sources, &[], DEFAULT_MIN_QUOTE_CHARS)
}

/// Verify against the turn's evidence UNIVERSE — the untruncated chunks — and
/// not merely against the budgeted prompt rendering of it.
///
/// # Why this exists
///
/// This module's contract, stated at the top of the file, is that a quoted
/// span is verified "against the document". The synthesis paths were instead
/// passing `doc_context`, which is `format_scored_chunks_with_kinds(&chunks,
/// budget)` — and that runs every chunk through
/// `truncate_chunk_content` → `MAX_CHUNK_CHARS` = 600. Chunks are ~2000 chars,
/// so the guard was reading roughly the first 30% of each one and calling the
/// rest absent.
///
/// Measured 2026-08-05 by replaying frozen bench transcripts through the real
/// deciders: **50 of 80 released citations (62.5%) shipped to the user as
/// `[unverified excerpt: …]`, and 55 of 55 of those spans are verbatim in the
/// turn's evidence. Zero were fabrications.** The discriminator is purely the
/// offset of the quote inside its own chunk — a kept span sat at offset 273, a
/// demoted one at 792, another at 1708. Telling a reader their own sources do
/// not support text that is sitting in those sources is a worse failure than
/// the composite-quote framing this guard was built to catch.
///
/// # What it does and does not widen
///
/// `chunks` is passed IN ADDITION to `evidence`, never instead of it, so the
/// source set is a strict superset of what the old call verified against: this
/// can only remove demotions, never add one. `evidence` still carries the
/// pieces that are not chunk text (the conversation briefing, the code-trace
/// block), so nothing that used to verify stops verifying.
///
/// The guard keeps its full strength on the failure it exists for. A composite
/// quote — real fragments spliced with `…` — is not contiguous in any chunk
/// under any normalisation, so it is still demoted. What stops being demoted is
/// exactly the class that was never fabricated to begin with.
///
/// The empty-`evidence` guard is unchanged and deliberately keyed on
/// `evidence`, not on `chunks`: the parametric / retrieval-miss path must keep
/// returning the answer untouched.
///
/// DO NOT "fix" this by raising `MAX_CHUNK_CHARS`. That constant is a
/// prompt-budget decision about how much of each chunk the SYNTHESIS model
/// reads, and re-costs every turn; the defect here was a verifier adopting the
/// prompt's rendering as its notion of what the sources say.
pub fn verify_answer_against_turn_evidence(
    answer: &str,
    evidence: &str,
    chunks: &[String],
) -> VerificationResult {
    if evidence.trim().is_empty() {
        return VerificationResult {
            rewritten: answer.to_string(),
            ..VerificationResult::default()
        };
    }
    let mut sources: Vec<String> = Vec::with_capacity(chunks.len() + 1);
    sources.push(evidence.to_string());
    sources.extend(chunks.iter().cloned());
    verify_quotes(answer, &sources, &[], DEFAULT_MIN_QUOTE_CHARS)
}

/// [`verify_answer_against_turn_evidence`], with the stored texts the turn's
/// chunks were cut from as sources too (ADDRESSED_TEXT §5.3, convergence
/// commit 2; `runtime::quote_surface` reads them).
///
/// A chunk is a re-joined, overlapped, title-headed cut of its document, so a
/// quote from the same document past the chunk's edge is verbatim source
/// text the chunks cannot see: the GR-19/20 class one level out. The texts
/// are listed FIRST, so a quote standing in one is addressed into it
/// ([`VerifiedQuote::source`] below `texts.len()`). The set is a superset of
/// the old one, so this only removes demotions; a composite stays
/// non-contiguous in a text as in a chunk. The empty-`evidence` guard is
/// unchanged.
pub fn verify_answer_against_turn_texts(
    answer: &str,
    evidence: &str,
    chunks: &[String],
    texts: &[&str],
) -> VerificationResult {
    if evidence.trim().is_empty() {
        return VerificationResult {
            rewritten: answer.to_string(),
            ..VerificationResult::default()
        };
    }
    let sources: Vec<&str> = texts
        .iter()
        .copied()
        .chain(std::iter::once(evidence))
        .chain(chunks.iter().map(String::as_str))
        .collect();
    verify_against(answer, &sources, DEFAULT_MIN_QUOTE_CHARS)
}

/// The aligner's knobs, compiled in from `quote-align/align.toml`; that
/// crate's `the_shipped_file_loads` pins that they load, so `None` is a build
/// defect. It fails closed: with no aligner no quote is verified, so every
/// checked quote is demoted, and the error says why once.
fn align_config() -> Option<&'static quote_align::AlignConfig> {
    static CFG: std::sync::OnceLock<Option<quote_align::AlignConfig>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| match quote_align::AlignConfig::shipped() {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::error!(error = %e, "quote guard: the shipped align.toml does not load; no quote can be verified");
            None
        }
    })
    .as_ref()
}

/// Where `quote` stands verbatim in `sources`, its edge ellipses trimmed
/// first ([`trim_edge_ellipses`]): [`align_exact`] with case kept — a
/// case-mismatched "quote" is not verbatim. An interior ellipsis the source
/// does not have is not in the source, so a spliced composite never
/// verifies.
pub(crate) fn locate_verbatim(
    quote: &str,
    sources: &[&str],
) -> Option<(usize, std::ops::Range<usize>)> {
    align_exact(trim_edge_ellipses(quote), sources)
}

/// The one aligner (`quote_align::align`, ADDRESSED_TEXT §5.3) in exact
/// mode: where `quote` stands verbatim in `sources`, as the first source in
/// order and its code points. The aligner says where the quote stands; it
/// stands there verbatim when its `norm_v0` form occurs in the source's
/// `norm_v0` form overlapping that stretch, and the range returned is
/// exactly that occurrence.
///
/// `norm_v0` is the fold this module used to spell itself (NFC; whitespace
/// runs, curly quotes, dashes and `…` folded; `*` `` ` `` `_` dropped; case
/// kept). Deciding on the folded strings, rather than on the aligner's
/// differences, keeps what a substring test accepted: a quote that starts or
/// ends inside a word, or quotes an ellipsis or a bracket the source itself
/// has (`every_stretch_of_a_source_is_located_where_it_stands`).
pub(crate) fn align_exact(
    quote: &str,
    sources: &[&str],
) -> Option<(usize, std::ops::Range<usize>)> {
    let needle = quote_align::norm_v0(quote);
    let n = needle.len();
    if n == 0 {
        return None;
    }
    let mut found = quote_align::align(quote, sources, align_config()?).alignments;
    found.sort_by_key(|a| (a.text, a.source.start));
    let mut hay: Option<(usize, quote_align::NormText)> = None;
    found.into_iter().find_map(|a| {
        if hay.as_ref().map(|(text, _)| *text) != Some(a.text) {
            hay = Some((a.text, quote_align::norm_v0(sources[a.text])));
        }
        let (_, hay) = hay.as_ref()?;
        // An occurrence overlapping the aligned stretch lies within the quote's
        // length of it either side, edge words the aligner left out included.
        let from = norm_position(hay, a.source.start).saturating_sub(n);
        let to = (norm_position(hay, a.source.end) + n).min(hay.len());
        let at = hay.chars()[from..to]
            .windows(n)
            .position(|w| w == needle.chars())?;
        Some((a.text, hay.origin(from + at..from + at + n)))
    })
}

/// The first normalised position of `hay` made from input code point `at` or
/// later (`hay.len()` when none is): `norm_v0`'s map back is in input order.
fn norm_position(hay: &quote_align::NormText, at: usize) -> usize {
    let (mut lo, mut hi) = (0, hay.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if hay.origin(mid..mid + 1).start < at {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// `quote` without its leading and trailing runs of `.` and whitespace, as
/// `norm_v0` sees them (so `…` counts). `"...the spectre took its crawl..."`
/// is honest edge-elision, not a composite — the elided part is OUTSIDE the
/// quoted span. Interior ellipses are untouched. The cut is made on the
/// quote's own characters through `norm_v0`'s map back to them.
fn trim_edge_ellipses(quote: &str) -> &str {
    let norm = quote_align::norm_v0(quote);
    let kept = |c: &char| *c != '.' && *c != ' ';
    let (Some(first), Some(last)) = (
        norm.chars().iter().position(kept),
        norm.chars().iter().rposition(kept),
    ) else {
        return "";
    };
    quote_align::code_point_slice(quote, norm.origin(first..last + 1)).unwrap_or("")
}

/// The fold this module once spelled itself, now `norm_v0`'s. Kept for the
/// unmodified witness `normalise_for_match_folds`, which pins that the
/// convergence folds exactly what the old one did on its cases.
#[cfg(test)]
fn normalise_for_match(s: &str) -> String {
    quote_align::norm_v0(s).to_string()
}

/// `true` if `c` is a double-quote character that opens a span we
/// should verify.
fn is_double_quote_open(c: char) -> bool {
    c == '"' || c == '\u{201C}' || c == '\u{201D}'
}

/// Find the next character index in `chars[from..]` that closes a
/// double-quote span. Mirrors `is_double_quote_open` for symmetric
/// detection — the model often mixes `"..."` and `"..."`.
/// The close of a quoted span, or `None` when a blank line comes first. A
/// quotation does not run across paragraphs: pairing across one let an unclosed
/// quote capture the next paragraph's opener, and the demotion then swallowed
/// everything between, a `Grounded in the source:` header included (2026-09-13
/// chaos soak, step 239).
fn find_double_quote_close(chars: &[char], from: usize) -> Option<usize> {
    let mut line_is_blank = false;
    for (offset, &c) in chars[from..].iter().enumerate() {
        match c {
            '"' | '\u{201C}' | '\u{201D}' => return Some(from + offset),
            '\n' if line_is_blank => return None,
            '\n' => line_is_blank = true,
            c if c.is_whitespace() => {}
            _ => line_is_blank = false,
        }
    }
    None
}

// The tests live in a sibling file, so this one stays under its size band
// (ARCH §3.1). `#[path]`, so the names are unchanged.
#[cfg(test)]
#[path = "quote_verification/tests.rs"]
mod tests;
