// SPDX-License-Identifier: AGPL-3.0-or-later
//! H4 deliverable 1 — span resolution: a claimed span resolves against the
//! sealed evidence, or it is typed [`SpanResolution::Unverified`]. There is no
//! third path, and in particular no silent pass.
//!
//! `NATIVE_GROUNDING.md` §5 H4: *"A segment claiming `Grounded{chunk_id, span}`
//! is verified by the deterministic kernel (`contains_ci` + the ≥2-word
//! verbatim-phrase shortcut that `value_present_in_chunks` already implements).
//! Resolution failure demotes the segment to `Unverified` — rendered
//! distinctly, never silently released as grounded."*
//!
//! # One decider for presence
//!
//! Whether a span is present in the evidence is decided in exactly one place:
//! [`value_present_in_chunks`] (`value_presence.rs:152`), the shipped kernel the
//! gate already uses to DECIDE and the chaos scorer already uses to MEASURE
//! `blatant_confab_rate`. This module does not re-derive it, does not wrap it in
//! a second threshold, and does not disagree with it — principle 8, and the
//! smell-table row "two implementations of one threshold, formula, or key".
//!
//! What this module adds on top is an **address**. `value_present_in_chunks`
//! answers yes/no over the pool joined into one haystack; a citation needs to
//! name *which chunk* and *where*. So resolution is two questions asked in
//! order:
//!
//! 1. **Is it present?** — `value_present_in_chunks`. No ⇒ `Unverified`. This is
//!    the verdict, and it is not this module's to make.
//! 2. **Can we address it?** — [`locate_verbatim`] looks for the span as one
//!    contiguous phrase inside a *single* chunk. Found ⇒ `Verbatim` with byte
//!    offsets into that chunk. Not found ⇒ `Fuzzy`: present, but scattered, so
//!    there is no honest span to point at.
//!
//! The locator is strictly stricter than the decider (single chunk, contiguous),
//! so it can never resolve something the kernel called absent. `Fuzzy` is
//! therefore the *only* place the two can differ, and it is a resolution-quality
//! distinction, not a second opinion about grounding.
//!
//! # Three ways to not resolve, not one
//!
//! [`UnverifiedReason`] separates `NotFound` (a judgement: we looked, it is not
//! there), `NoEvidence` (a refusal to judge: there was nothing to look in) and
//! `Empty` (a malformed claim). ARCH §18.3 — absence is reported, never
//! defaulted; collapsing "could not judge" into "failed" is the smell this
//! avoids. A caller that treats all three alike is free to; a caller that must
//! distinguish a fabrication from a missing evidence pool can.
//!
//! # Determinism
//!
//! Pure function of `(span, chunks)`. No model, no clock, no allocation-order
//! dependence. This is what makes the H4 replay a HARD verdict under §7.4.

use crate::runtime::value_present_in_chunks;

/// Why a claimed span did not resolve. Three verdicts, deliberately not one —
/// see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiedReason {
    /// We looked in a non-empty evidence pool and the span is not there. The
    /// fabrication verdict.
    NotFound,
    /// There was no evidence to look in (empty pool, or every chunk blank).
    /// **Could-not-judge**, not "failed" — a turn with no retrieval has not
    /// been convicted of anything.
    NoEvidence,
    /// The claimed span was empty or whitespace-only. A malformed claim, which
    /// is a bug in whatever produced it rather than a fact about the evidence.
    Empty,
}

/// The outcome of resolving one claimed span against the sealed evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpanResolution {
    /// Present as one contiguous phrase inside a single chunk, with an address.
    /// `start..end` are **byte offsets into `chunks[chunk]` as given** — slicing
    /// the original chunk with them yields the matched text (modulo the case and
    /// whitespace folding the match allows).
    Verbatim {
        /// Index into the `chunks` slice that was passed in.
        chunk: usize,
        /// Byte offset of the first matched character in that chunk.
        start: usize,
        /// Byte offset one past the last matched character in that chunk.
        end: usize,
    },
    /// Present in the evidence by the shipped presence kernel, but not as a
    /// contiguous phrase in any single chunk — so there is no span to cite. A
    /// grounded *claim* without a grounded *address*.
    Fuzzy,
    /// Did not resolve. Never a silent pass.
    Unverified {
        /// Which of the three non-resolutions this is.
        reason: UnverifiedReason,
    },
}

impl SpanResolution {
    /// True when the evidence supports the span at all (`Verbatim` or `Fuzzy`).
    ///
    /// Note what this deliberately does NOT do: it does not treat
    /// `Unverified { NoEvidence }` as a resolution. A could-not-judge is not a
    /// pass.
    pub fn is_resolved(&self) -> bool {
        matches!(self, Self::Verbatim { .. } | Self::Fuzzy)
    }

    /// The chunk this span was addressed to, when it has an address.
    pub fn chunk(&self) -> Option<usize> {
        match self {
            Self::Verbatim { chunk, .. } => Some(*chunk),
            _ => None,
        }
    }

    /// A short stable label for artifacts and tracing. One name per outcome.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Verbatim { .. } => "verbatim",
            Self::Fuzzy => "fuzzy",
            Self::Unverified {
                reason: UnverifiedReason::NotFound,
            } => "unverified_not_found",
            Self::Unverified {
                reason: UnverifiedReason::NoEvidence,
            } => "unverified_no_evidence",
            Self::Unverified {
                reason: UnverifiedReason::Empty,
            } => "unverified_empty",
        }
    }
}

/// Resolve one claimed span against the sealed evidence chunks.
///
/// See the module docs for the two-question shape and why presence has exactly
/// one decider. Deterministic and allocation-bounded; no model is consulted.
pub fn resolve_span(span: &str, chunks: &[String]) -> SpanResolution {
    if span.trim().is_empty() {
        return SpanResolution::Unverified {
            reason: UnverifiedReason::Empty,
        };
    }
    // Empty pool AND all-blank pool are the same refusal: there is nothing to
    // look in. `[].iter().all(..)` is true, so this covers both.
    if chunks.iter().all(|c| c.trim().is_empty()) {
        return SpanResolution::Unverified {
            reason: UnverifiedReason::NoEvidence,
        };
    }

    // Question one, asked of the ONE decider.
    if !value_present_in_chunks(span, chunks) {
        return SpanResolution::Unverified {
            reason: UnverifiedReason::NotFound,
        };
    }

    // Question two: can we point at it?
    match locate_verbatim(span, chunks) {
        Some((chunk, start, end)) => SpanResolution::Verbatim { chunk, start, end },
        None => SpanResolution::Fuzzy,
    }
}

/// Find `span` as one contiguous phrase inside a single chunk, returning
/// `(chunk_index, start_byte, end_byte)` in the ORIGINAL chunk text.
///
/// The one aligner (ADDRESSED_TEXT §5.3) in exact mode
/// (`quote_verification::align_exact`) over case-folded copies of both
/// sides (`quote_verification::fold_case`): this caller ignores case, because
/// chunk text carries the corpus's own capitals and wrapping ("Karl\n\nYundt")
/// and a citation should survive them. Whitespace runs, curly quotes, dashes
/// and emphasis markers fold inside `norm_v0`; other punctuation does not, so
/// a spliced composite phrase still fails, as it does for quoted spans.
///
/// First match wins, scanning chunks in order, so the result is a deterministic
/// function of the inputs.
fn locate_verbatim(span: &str, chunks: &[String]) -> Option<(usize, usize, usize)> {
    use crate::quote_verification::{align_exact, fold_case};
    let folded: Vec<String> = chunks.iter().map(|c| fold_case(c)).collect();
    let texts: Vec<&str> = folded.iter().map(String::as_str).collect();
    let (i, range) = align_exact(&fold_case(span.trim()), &texts)?;
    let chunk = &chunks[i];
    Some((i, byte_at(chunk, range.start), byte_at(chunk, range.end)))
}

/// The byte offset of code point `cp` in `s` (its length when past the end).
fn byte_at(s: &str, cp: usize) -> usize {
    s.char_indices().nth(cp).map_or(s.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One Conrad passage, carrying the corpus's own hard wrapping. The blank
    /// line inside it is not incidental — it is what the chunk store actually
    /// stores, and a resolver that cannot see through it cannot cite anything.
    fn chunks() -> Vec<String> {
        vec![
            "Karl Yundt giggled grimly, and Comrade Alexander Ossipon,\n\nnicknamed the Doctor, \
             sat near Mr Verloc."
                .to_string(),
            "Sir Ethelred received the Assistant Commissioner in the small hours.".to_string(),
        ]
    }

    // ── the order's two required cases ───────────────────────────────────────

    #[test]
    fn a_span_absent_from_the_evidence_does_not_resolve() {
        // "Vladimir Stepanovich Haldin" is the spec's own worked example of a
        // multi-part invention: no part of it is in this evidence.
        let r = resolve_span("Vladimir Stepanovich Haldin", &chunks());
        assert_eq!(
            r,
            SpanResolution::Unverified {
                reason: UnverifiedReason::NotFound
            },
            "an invented span must be typed Unverified, not silently passed"
        );
        assert!(!r.is_resolved());
    }

    #[test]
    fn the_same_span_present_resolves() {
        let r = resolve_span("Karl Yundt giggled grimly", &chunks());
        assert!(
            r.is_resolved(),
            "a span verbatim in the evidence must resolve"
        );
        assert_eq!(r.chunk(), Some(0));
        assert!(matches!(r, SpanResolution::Verbatim { .. }));
    }

    // ── the address is real ──────────────────────────────────────────────────

    #[test]
    fn the_verbatim_address_indexes_the_chunk_it_names() {
        let cs = chunks();
        let SpanResolution::Verbatim { chunk, start, end } =
            resolve_span("Assistant Commissioner", &cs)
        else {
            panic!("expected a verbatim resolution with an address");
        };
        assert_eq!(
            chunk, 1,
            "the span lives in the second chunk, not the first"
        );
        assert_eq!(
            &cs[chunk][start..end],
            "Assistant Commissioner",
            "start..end must slice the ORIGINAL chunk back to the span"
        );
    }

    #[test]
    fn an_address_survives_the_corpus_own_line_wrapping() {
        // The span straddles the "\n\n" the chunk store put there.
        let cs = chunks();
        let SpanResolution::Verbatim { chunk, start, end } =
            resolve_span("comrade alexander ossipon, nicknamed the doctor", &cs)
        else {
            panic!("a span split by a paragraph break must still address");
        };
        assert_eq!(chunk, 0);
        let sliced = &cs[chunk][start..end];
        assert!(
            sliced.starts_with("Comrade Alexander") && sliced.ends_with("the Doctor"),
            "sliced back: {sliced:?}"
        );
    }

    // ── present-but-unaddressable is its own outcome ─────────────────────────

    #[test]
    fn a_span_whose_words_are_scattered_is_fuzzy_not_verbatim() {
        // Both words are in chunk 0, but never adjacent — the presence kernel
        // says yes, the locator has nothing honest to point at.
        let r = resolve_span("Yundt Ossipon", &chunks());
        assert_eq!(
            r,
            SpanResolution::Fuzzy,
            "scattered presence must not be dressed up as an address"
        );
        assert!(r.is_resolved());
        assert_eq!(r.chunk(), None, "Fuzzy has no address to hand out");
    }

    // ── the three non-resolutions stay three ─────────────────────────────────

    #[test]
    fn an_empty_evidence_pool_is_could_not_judge_not_not_found() {
        // ARCH §18.3: absence reported, never defaulted. A turn that retrieved
        // nothing has not been convicted of fabricating.
        assert_eq!(
            resolve_span("Karl Yundt", &[]),
            SpanResolution::Unverified {
                reason: UnverifiedReason::NoEvidence
            }
        );
        assert_eq!(
            resolve_span("Karl Yundt", &["   \n ".to_string()]),
            SpanResolution::Unverified {
                reason: UnverifiedReason::NoEvidence
            },
            "an all-blank pool is the same refusal as no pool"
        );
    }

    #[test]
    fn an_empty_span_is_malformed_not_absent() {
        assert_eq!(
            resolve_span("   ", &chunks()),
            SpanResolution::Unverified {
                reason: UnverifiedReason::Empty
            }
        );
    }

    #[test]
    fn every_outcome_has_its_own_label() {
        use std::collections::HashSet;
        let labels: HashSet<&str> = [
            SpanResolution::Verbatim {
                chunk: 0,
                start: 0,
                end: 1,
            },
            SpanResolution::Fuzzy,
            SpanResolution::Unverified {
                reason: UnverifiedReason::NotFound,
            },
            SpanResolution::Unverified {
                reason: UnverifiedReason::NoEvidence,
            },
            SpanResolution::Unverified {
                reason: UnverifiedReason::Empty,
            },
        ]
        .iter()
        .map(|r| r.label())
        .collect();
        assert_eq!(labels.len(), 5, "one name per outcome, no collisions");
    }

    // ── the presence decider is not second-guessed ───────────────────────────

    #[test]
    fn the_locator_never_resolves_what_the_kernel_called_absent() {
        // The locator is strictly stricter (single chunk, contiguous), so for
        // every span it addresses, the kernel must already agree it is present.
        let cs = chunks();
        for span in [
            "Karl Yundt",
            "Assistant Commissioner",
            "Vladimir Stepanovich Haldin",
            "Verloc",
            "the Doctor",
            "Ossipon sat near",
        ] {
            let located = super::locate_verbatim(span, &cs).is_some();
            if located {
                assert!(
                    value_present_in_chunks(span, &cs),
                    "{span:?} located but the presence kernel says absent — the two \
                     have diverged, which is the thing this module promises cannot happen"
                );
            }
        }
    }

    /// The locator folds what the quote guard folds (`norm_v0`, through the
    /// one aligner), so a span written with a straight apostrophe and a
    /// hyphen is addressed in a source that has the typographic ones, at the
    /// source's own bytes. Red against the old whitespace-and-case fold, which
    /// called this present but unaddressable (`Fuzzy`).
    #[test]
    fn a_span_with_straight_quotes_is_addressed_in_a_typographic_source() {
        let cs = vec!["It was Mr Verloc\u{2019}s shop\u{2014}small and dim.".to_string()];
        let r = resolve_span("Mr Verloc's shop-small", &cs);
        let SpanResolution::Verbatim { chunk, start, end } = r else {
            panic!("expected an address, got {r:?}");
        };
        assert_eq!(
            &cs[chunk][start..end],
            "Mr Verloc\u{2019}s shop\u{2014}small"
        );
    }

    // ── determinism (§7.4: HARD verdicts come from deterministic facets) ─────

    #[test]
    fn resolution_is_a_pure_function_of_its_inputs() {
        let cs = chunks();
        for span in ["Karl Yundt", "Yundt Ossipon", "nobody at all", "", "  "] {
            let a = resolve_span(span, &cs);
            let b = resolve_span(span, &cs);
            assert_eq!(a, b, "repeat resolution of {span:?} diverged");
        }
    }
}
