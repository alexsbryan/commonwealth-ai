// SPDX-License-Identifier: AGPL-3.0-or-later
//! A Project Gutenberg text's wrapper: the distributor's header, up to and
//! including its `*** START OF … PROJECT GUTENBERG …` line, and its licence,
//! from its `*** END OF …` line on (OICP v0.6 §2.11, PFM). Neither is the
//! work's word, so neither yields a sentence for search or for verify's
//! candidates. The stored text, its name and every span into it are
//! unchanged; only which of its sentences are indexed moves.
//!
//! Why: in zoracite's K2 run, 258 of 346 `contradicted` flags quoted the
//! header ("Title:", "Release date") as a source saying otherwise
//! (zoracite c8f02e3, `cargo xtask audit claims front-matter`).

use std::ops::Range;

/// The bytes of `text` that are the work: after a Gutenberg start marker's
/// line and before an end marker's line. A text with neither is all work;
/// a text with one is cut on that side alone.
pub fn body(text: &str) -> Range<usize> {
    let mut start = 0;
    let mut end = text.len();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let next = at + line.len();
        if start == 0 && marks(line, "START OF") {
            start = next;
        } else if marks(line, "END OF") && at >= start {
            end = at;
            break;
        }
        at = next;
    }
    if start > 0 || end < text.len() {
        tracing::trace!(target: "sovereign_daemon::sentence_index", start, end, len = text.len(), "sentence index: a Gutenberg wrapper left out");
    }
    start..end
}

/// Whether `line` is a Gutenberg marker of `kind` (`*** START OF THE PROJECT
/// GUTENBERG EBOOK …`, and the older `THIS PROJECT GUTENBERG` form).
fn marks(line: &str, kind: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("***")
        && l.trim_start_matches('*').trim_start().starts_with(kind)
        && l.to_ascii_uppercase().contains("GUTENBERG")
}

#[cfg(test)]
mod tests {
    use super::*;

    const WRAPPED: &str = "The Project Gutenberg eBook of Common Sense\n\
        Title: Common Sense\nRelease date: June 1, 1997\n\
        *** START OF THE PROJECT GUTENBERG EBOOK COMMON SENSE ***\n\
        Society is produced by our wants. Government by our wickedness.\n\
        *** END OF THE PROJECT GUTENBERG EBOOK COMMON SENSE ***\n\
        Updated editions will replace the previous one.\n";

    #[test]
    fn a_gutenberg_text_s_body_is_between_its_markers() {
        let b = body(WRAPPED);
        assert_eq!(
            &WRAPPED[b],
            "Society is produced by our wants. Government by our wickedness.\n"
        );
    }

    #[test]
    fn a_text_with_no_wrapper_is_all_work() {
        let t = "Society is produced by our wants.\n*** a line of stars ***\n";
        assert_eq!(body(t), 0..t.len());
    }

    #[test]
    fn the_sentence_index_cuts_no_sentence_from_the_wrapper() {
        let rows = super::super::store::split(WRAPPED);
        let cut: Vec<&str> = rows
            .iter()
            .map(|r| &WRAPPED[r.byte_start as usize..r.byte_end as usize])
            .collect();
        assert!(!cut.is_empty());
        assert!(
            cut.iter().all(|s| !s.contains("Release date")
                && !s.contains("GUTENBERG")
                && !s.contains("Updated editions")),
            "{cut:?}"
        );
        let plain = "Society is produced by our wants. Government by our wickedness.";
        assert_eq!(super::super::store::split(plain).len(), 2);
    }
}
