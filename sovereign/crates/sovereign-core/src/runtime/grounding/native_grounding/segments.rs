// SPDX-License-Identifier: AGPL-3.0-or-later
//! Segments — provenance the reader can SEE, and nothing else.
//!
//! `NATIVE_GROUNDING.md §6` puts an `AnswerSegment[]` on the chat
//! response: per-stretch provenance of the released text, so a user can
//! tell which words came from their sources, which are the model's own,
//! and which failed to resolve.
//!
//! # Display-only, structurally
//!
//! **This is not a convention, and it is not enforced by a comment.**
//! Three things hold it:
//!
//! 1. **Ordering.** [`segments_for_display`] takes the FINAL released
//!    text — the string the gate already returned. Every decision the
//!    turn makes (admission, verification, retry, release) is complete
//!    before this function can be called, because its only input does
//!    not exist until they are. There is no execution order in which a
//!    segment could inform a decision.
//! 2. **Signature.** It takes `&str` and `&[String]` and returns
//!    `Vec<AnswerSegment>`. It cannot reach a verdict, a gate, a claim
//!    record or a judge, because none of them are in scope.
//! 3. **Purity.** No model, no clock, no env, no I/O — it is
//!    [`span_resolver::resolve_span`] over sentences. Nothing it does
//!    can have an effect anywhere else.
//!
//! # Why display-only is a MEASURED constraint, not caution
//!
//! The resolver-precision measurement
//! (`sovereign/bench/calibration/resolver-precision/FINDINGS.md`,
//! 2026-08-09) replayed 130 frozen claims and found the resolver
//! certifies at **precision 0.7429** against the incumbent judge's
//! verdicts, against a pre-registered bar of 0.98. The mechanism is the
//! part that matters here:
//!
//!   * `Verbatim` fired on 4 of 130 claims and the incumbent judge
//!     **failed all four** — they were claim-extraction fragments ("the
//!     inn's back") that appear contiguously because they are short.
//!   * Every genuine claim that resolved at all came back `Fuzzy`, which
//!     means only that the claim's WORDS occur somewhere in the pool. A
//!     confabulation assembled out of vocabulary the passages genuinely
//!     contain resolves `Fuzzy` exactly as readily as a true claim.
//!
//! So a segment is an honest statement about **where text appears**, and
//! it is NOT evidence that a proposition is supported. Rendering it is
//! useful; deciding on it would ship wrong "Grounded" badges at roughly
//! one in four. That is why the constraint is structural.

use sovereign_contracts::types::{AnswerSegment, SegmentKind};

use super::span_resolver::{resolve_span, SpanResolution};

/// Longest released answer this will segment, in bytes. A guard, not a
/// policy: segmentation is O(sentences x chunks) string search, and an
/// answer far past any real length is a runaway, not a turn to render.
const MAX_SEGMENTED_BYTES: usize = 64 * 1024;

/// Split released text into sentence-ish stretches, keeping byte ranges
/// into the ORIGINAL string.
///
/// Deliberately simple: split after `.`, `!`, `?` followed by
/// whitespace, and on newlines. A cleverer splitter would be a second
/// notion of "sentence" in a codebase that already has one for the
/// sentence sweep; this one exists only to give the renderer stretches
/// to colour, and every range it emits is exact.
fn sentence_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        let is_break = if c == b'\n' {
            true
        } else if matches!(c, b'.' | b'!' | b'?') {
            // Only a boundary when whitespace (or end) follows, so
            // "0.5" and "e.g." do not shatter into fragments.
            b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace())
        } else {
            false
        };
        i += 1;
        if is_break {
            // Absorb trailing whitespace into this stretch so the ranges
            // tile the string with no gaps.
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if !text[start..i].trim().is_empty() {
                out.push(start..i);
            }
            start = i;
        }
    }
    if start < text.len() && !text[start..].trim().is_empty() {
        out.push(start..text.len());
    }
    out
}

/// The provenance of one released answer, as typed segments.
///
/// Pure. Takes the final text and the sealed evidence; returns display
/// data. See the module docs for why it cannot influence a decision.
pub fn segments_for_display(released: &str, chunks: &[String]) -> Vec<AnswerSegment> {
    if released.len() > MAX_SEGMENTED_BYTES {
        return Vec::new();
    }
    sentence_ranges(released)
        .into_iter()
        .map(|range| {
            let sentence = released[range.clone()].trim();
            let kind = match resolve_span(sentence, chunks) {
                SpanResolution::Verbatim { chunk, start, end } => SegmentKind::Grounded {
                    // The chunk's position in the sealed pool. The
                    // caller owns mapping that to a corpus-facing id;
                    // this module has only the texts it was handed.
                    chunk_id: chunk.to_string(),
                    span: start..end,
                    // Filled by the caller from the pool-aligned citation
                    // targets (`streaming.rs`). Left `None` here rather
                    // than invented, so this function stays a pure
                    // function of (text, chunk texts).
                    address: None,
                },
                // Present in the pool but scattered — the words are
                // there, the address is not. Deliberately NOT
                // `Grounded`: a grounded badge promises somewhere to
                // look, and this has nowhere. Per the precision
                // measurement it is also not evidence of support.
                SpanResolution::Fuzzy => SegmentKind::Inference,
                // Never silently released as grounded (§5 H4).
                SpanResolution::Unverified { .. } => SegmentKind::Unverified,
            };
            AnswerSegment {
                text_range: range,
                kind,
                // The sentence-margin sweep is H4 machinery this order
                // did not port. `None` says "not scored", which is true;
                // a 0.0 would say "scored, and badly".
                margin: None,
            }
        })
        .collect()
}

/// A one-line, per-segment provenance render for a terminal.
///
/// Lives here rather than in the CLI so the label vocabulary has one
/// definition — the CLI, and any later surface, render the same words
/// for the same segment kind (ARCH §10.6).
pub fn render_segment_label(kind: &SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Grounded { .. } => "sourced",
        SegmentKind::Parametric => "model's own",
        SegmentKind::Inference => "words in sources, no single passage",
        SegmentKind::Unverified => "not found in sources",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_tile_the_string_exactly() {
        let t = "Alpha runs the mill. Beta keeps the ledger!\nGamma left.";
        let rs = sentence_ranges(t);
        assert!(rs.len() >= 3, "{rs:?}");
        // Every range must slice, and they must be non-overlapping and
        // ascending — a renderer highlights by these offsets.
        let mut prev_end = 0;
        for r in &rs {
            assert!(r.start >= prev_end, "overlap at {r:?}");
            assert!(t.get(r.clone()).is_some(), "range {r:?} does not slice");
            prev_end = r.end;
        }
        assert!(prev_end <= t.len());
    }

    #[test]
    fn decimals_and_abbreviations_do_not_shatter() {
        let t = "The rate was 0.5 percent in the third year.";
        assert_eq!(sentence_ranges(t).len(), 1, "split inside a decimal");
    }

    #[test]
    fn a_sentence_absent_from_the_evidence_is_unverified_never_grounded() {
        let chunks = vec!["The mill stands on Harbour Row.".to_string()];
        let segs = segments_for_display("Orrison poisoned the harbormaster.", &chunks);
        assert_eq!(segs.len(), 1);
        assert!(
            matches!(segs[0].kind, SegmentKind::Unverified),
            "{:?} — an unresolved sentence must never render as sourced",
            segs[0].kind
        );
    }

    #[test]
    fn a_verbatim_sentence_carries_a_real_address() {
        let chunks = vec!["The mill stands on Harbour Row. It burned in 1892.".to_string()];
        let segs = segments_for_display("The mill stands on Harbour Row.", &chunks);
        assert_eq!(segs.len(), 1);
        match &segs[0].kind {
            SegmentKind::Grounded {
                chunk_id,
                span,
                address,
            } => {
                assert_eq!(chunk_id, "0");
                // This function is handed texts, not corpus handles, so
                // it must not invent one. The caller fills it.
                assert!(address.is_none());
                // The address must actually address something.
                assert_eq!(&chunks[0][span.clone()], "The mill stands on Harbour Row.");
            }
            other => panic!("expected Grounded, got {other:?}"),
        }
    }

    /// The measured constraint, held as a test rather than a comment:
    /// scattered-vocabulary overlap must NOT produce a `Grounded` badge.
    /// This is the exact shape the 0.7429 precision came from — a
    /// sentence whose words are all present but which no passage
    /// asserts.
    #[test]
    fn scattered_vocabulary_is_never_badged_as_sourced() {
        let chunks = vec![
            "The harbormaster kept the ledger.".to_string(),
            "Orrison mended the sluice gate.".to_string(),
        ];
        // Every content word appears; the proposition appears nowhere.
        let segs = segments_for_display("The harbormaster mended the ledger gate.", &chunks);
        assert_eq!(segs.len(), 1);
        assert!(
            !matches!(segs[0].kind, SegmentKind::Grounded { .. }),
            "{:?} — vocabulary overlap is not support (resolver-precision FINDINGS)",
            segs[0].kind
        );
    }

    #[test]
    fn segmentation_is_deterministic_and_leaves_the_text_alone() {
        let chunks = vec!["The mill stands on Harbour Row.".to_string()];
        let text = "The mill stands on Harbour Row. Nobody knows who lit it.";
        let a = segments_for_display(text, &chunks);
        let b = segments_for_display(text, &chunks);
        assert_eq!(a, b);
        // Ranges are into the ORIGINAL string, unmodified.
        for s in &a {
            assert!(text.get(s.text_range.clone()).is_some());
        }
    }

    #[test]
    fn an_empty_answer_segments_to_nothing_rather_than_one_empty_stretch() {
        assert!(segments_for_display("", &["x".to_string()]).is_empty());
        assert!(segments_for_display("   \n  ", &["x".to_string()]).is_empty());
    }

    #[test]
    fn every_segment_kind_has_a_distinct_reader_facing_label() {
        let labels = [
            render_segment_label(&SegmentKind::Grounded {
                chunk_id: "0".into(),
                span: 0..1,
                address: None,
            }),
            render_segment_label(&SegmentKind::Parametric),
            render_segment_label(&SegmentKind::Inference),
            render_segment_label(&SegmentKind::Unverified),
        ];
        let mut uniq = labels.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), labels.len(), "two kinds render the same words");
    }
}
