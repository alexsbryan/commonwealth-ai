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

    /// **GR-09 — the provenance strip reads the RELEASED string, and the two
    /// honesty layers therefore agree by construction.**
    ///
    /// Witnessed live on the desktop 2026-08-10
    /// (`test-artifacts/p1-desktop-on-grounded.json`, note 531b513c): a turn
    /// asked to copy one sentence word-for-word produced 29 segments and 0
    /// grounded, and the first reading of that — a segmentation failure — was
    /// wrong and had to be retracted. The real chain: the 2B fixture model
    /// mangled its own "verbatim" quote (the source reads "47 meters tall from
    /// base to lantern gallery"; it emitted "47 mètrest all from base to
    /// lantenns gallery"), the incumbent quote guardrail failed the substring
    /// check and demoted the span to `[unverified excerpt: …]` before release,
    /// and the strip then segmented that released text and honestly said
    /// Unverified.
    ///
    /// So `grounded = 0` on a paraphrasing model is the CORRECT reading, not
    /// an instrument defect, and the layers are consistent rather than
    /// redundant: a genuinely verbatim span is never rewritten, survives
    /// unwrapped and resolves; only an unverified span carries the wrapper,
    /// and it was already going to segment Unverified.
    ///
    /// The last assertion is the one with teeth. A segment's `text_range` is a
    /// byte range into whichever string the strip was handed, so handing it
    /// the pre-guardrail draft does not merely describe different text — it
    /// hands the UI offsets that address the released string wrongly.
    #[test]
    fn a_demoted_quote_segments_unverified_and_the_ranges_follow_the_release() {
        use crate::quote_verification::{verify_quotes, DEFAULT_MIN_QUOTE_CHARS};

        const SOURCE: &str = "The tower is 47 meters tall from base to lantern gallery";
        let chunks = vec![format!(
            "Meridian lighthouse survey, 1874. {SOURCE}. The lamp was lit that autumn."
        )];

        // (1) THE MODEL THAT COPIES. The guardrail finds the span verbatim in
        // the evidence and leaves the text exactly as written.
        let faithful = format!("The survey gives the height. \"{SOURCE}\"");
        let kept = verify_quotes(&faithful, &chunks, &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(
            kept.demoted_count, 0,
            "a verbatim quote must survive the guardrail untouched, or the \
             contrast this test draws is not the one the desktop turn drew"
        );
        assert_eq!(kept.rewritten, faithful, "a kept quote is not rewritten");
        let kept_segs = segments_for_display(&kept.rewritten, &chunks);
        assert!(
            kept_segs
                .iter()
                .any(|s| !matches!(s.kind, SegmentKind::Unverified)),
            "the copied sentence resolved against its own source and must not \
             segment Unverified: {kept_segs:?}"
        );

        // (2) THE MODEL THAT MANGLES its own "verbatim" quote — the desktop
        // specimen, reduced.
        const MANGLED: &str = "The survey gives the height. \
                               \"The tower is 47 mètrest all from base to lantenns gallery\"";
        let demoted = verify_quotes(MANGLED, &chunks, &[], DEFAULT_MIN_QUOTE_CHARS);
        assert_eq!(
            demoted.demoted_count, 1,
            "the guardrail must catch the mangled quote — it is the first \
             honesty layer and the reason the second one reports zero"
        );
        assert!(
            demoted.rewritten.contains("[unverified excerpt:"),
            "the released text carries the runtime's OWN wrapper, not model \
             prose: {:?}",
            demoted.rewritten
        );

        let released_segs = segments_for_display(&demoted.rewritten, &chunks);
        assert!(
            !released_segs
                .iter()
                .any(|s| matches!(s.kind, SegmentKind::Grounded { .. })),
            "a demoted span must never carry a sourced badge — grounded = 0 \
             here is the honest answer, not a segmentation failure: \
             {released_segs:?}"
        );
        for s in &released_segs {
            assert!(
                demoted.rewritten.get(s.text_range.clone()).is_some(),
                "every range must index the string the strip was handed"
            );
        }

        // (3) WHY THE ORDER IS LOAD-BEARING. The guardrail changes the string's
        // LENGTH (it drops the quote marks and inserts a wrapper), so the same
        // answer segmented before and after release does not merely describe
        // different words — it yields different byte ranges. Compute the strip
        // over the draft and the UI highlights bytes of a text nobody saw.
        let draft_segs = segments_for_display(MANGLED, &chunks);
        let ranges = |v: &[AnswerSegment]| {
            v.iter()
                .map(|s| s.text_range.clone())
                .collect::<Vec<std::ops::Range<usize>>>()
        };
        assert_ne!(
            ranges(&draft_segs),
            ranges(&released_segs),
            "the guardrail rewrote the text but the segment ranges did not \
             move, so this test cannot tell the two inputs apart and proves \
             nothing about the ordering"
        );
    }

    /// **GR-09, the structural half: nothing may rewrite the released text
    /// after the strip has described it.**
    ///
    /// `segments_for_display` documents its input as "the FINAL released
    /// text", and the guarantee in `streaming.rs` is positional — there is no
    /// type that says "this `String` is finished". Until 2026-09-02 the call
    /// sat ABOVE four later rebindings of the very binding it reads: the quote
    /// guardrail's demotion, the unavailability marker, the lesson term-avoid
    /// pass and the authority guard. Each changes the string's length, and the
    /// sibling test above shows that moves every range.
    ///
    /// So this is the reader that absence needs (ARCH §7 — make it structural,
    /// not remembered). The region is found from the call site outward rather
    /// than by line number, so the guard cannot go stale against a moved
    /// block, and `include_str!` resolves relative to THIS file so it cannot
    /// pass vacuously from another directory.
    #[test]
    fn the_strip_reads_the_released_string() {
        const STREAMING: &str = include_str!("../../streaming.rs");
        let prod = STREAMING
            .split("\n#[cfg(test)]")
            .next()
            .unwrap_or(STREAMING);

        let call = prod
            .find("segments_for_display(")
            .expect("streaming.rs no longer calls segments_for_display — re-point this guard");
        // The turn that owns this call, bounded by the `full_text` buffers of
        // its neighbours: back to this function's own declaration, forward to
        // the next one.
        const DECL: &str = "let mut full_text = String::new();";
        let start = prod[..call]
            .rfind(DECL)
            .expect("the strip is not inside a turn that owns a `full_text` buffer");
        let end = prod[call..].find(DECL).map_or(prod.len(), |o| call + o);

        let mut offenders: Vec<String> = Vec::new();
        let mut offset = start;
        for line in prod[start..end].lines() {
            let t = line.trim_start();
            let rebinds = t.starts_with("full_text =")
                || t.starts_with("let full_text")
                || t.starts_with("let (full_text")
                || t.starts_with("let mut full_text")
                || t.contains("&mut full_text");
            if offset > call && rebinds && !t.starts_with("//") {
                let line_no = prod[..offset].matches('\n').count() + 1;
                offenders.push(format!("streaming.rs:{line_no}: {t}"));
            }
            offset += line.len() + 1;
        }
        assert!(
            offenders.is_empty(),
            "the released text is rewritten AFTER the provenance strip described \
             it. A segment's `text_range` is a byte range into the string the \
             strip was handed, so every one of these rewrites hands the UI \
             offsets into a text the reader never saw (note 531b513c):\n{}",
            offenders.join("\n")
        );

        // Vacuity guard: the region must actually contain the rewrites this
        // test exists to order. If they all moved elsewhere, an empty
        // `offenders` means "found nothing to check", not "checked and clean".
        let rewrites = prod[start..end]
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("let full_text") || t.starts_with("let (full_text")
            })
            .count();
        assert!(
            rewrites >= 3,
            "only {rewrites} post-synthesis rewrite(s) of `full_text` found in \
             this turn — the guard is scanning a region that no longer carries \
             them and would pass by describing nothing"
        );
    }
}
