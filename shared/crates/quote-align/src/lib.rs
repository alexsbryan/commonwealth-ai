// SPDX-License-Identifier: AGPL-3.0-or-later
#![warn(missing_docs)]
//! Where a quotation stands in a text, and how it differs from it.
//!
//! The one quote aligner (ADDRESSED_TEXT §5.3). It takes a quotation and some
//! `&str` texts and returns, for each place the quotation stands, the range
//! of text it covers and every difference between the two, in code points of
//! the inputs as given. It knows nothing of corpora, chunks or wires; the
//! evidence route maps its types onto `oicp_types::evidence`.
//!
//! 1. **Normalise** both sides with [`norm_v0`], which keeps a map from every
//!    normalised character back to the input code points it came from.
//! 2. **Seed** candidate windows at exact occurrences of the quotation's
//!    rarest word n-grams, each gap-free run of words seeding with n-grams of
//!    its own length, capped at 3.
//! 3. **Align** tokens inside each window by a weighted edit alignment that
//!    may drift [`AlignConfig::band`] tokens off the seed's diagonal. An
//!    ellipsis in the quotation is a free gap, reported as `Elided`; square
//!    brackets are editorial, reported as `Bracketed`.
//! 4. **Keep** alignments whose coverage reaches
//!    [`AlignConfig::coverage_floor`].
//! 5. **Bind exactly**: every range is into the caller's own string, so the
//!    caller slices its exact text from it.
//!
//! Nothing here traces: the crate links no logger by design. Every decision
//! is in the value returned ([`Aligned`] counts the anchors tried and the
//! alignments the floor dropped), so the caller logs it.

mod align;
mod config;
mod norm;
mod tokens;

pub use align::{align, Aligned, QuoteAlignment, QuoteEdit, QuoteEditKind};
pub use config::{AlignConfig, AlignConfigError};
pub use norm::{norm_v0, NormText};

/// The aligner's identity, as `aligner` on the wire. It names the alignment
/// algorithm and the normaliser. It changes whenever the same input could
/// align differently, so a client keys cached alignments on it; the golden
/// bank (`bank/golden.txt`) fails when the output changes and this does not.
pub const ALIGNER_ID: &str = "align/2 norm/0";

/// `s`'s code points `[range.start, range.end)`, or `None` when the range
/// falls outside `s` or is reversed.
pub fn code_point_slice(s: &str, range: std::ops::Range<usize>) -> Option<&str> {
    if range.start > range.end {
        return None;
    }
    let mut bounds = s
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(s.len()));
    let start = bounds.nth(range.start)?;
    let end = if range.end == range.start {
        start
    } else {
        bounds.nth(range.end - range.start - 1)?
    };
    Some(&s[start..end])
}

#[cfg(test)]
#[path = "golden_tests.rs"]
mod golden_tests;

#[cfg(test)]
mod tests {
    use super::code_point_slice;

    #[test]
    fn code_point_slices_count_characters_not_bytes() {
        let s = "café au lait";
        assert_eq!(code_point_slice(s, 0..4), Some("café"));
        assert_eq!(code_point_slice(s, 5..7), Some("au"));
        assert_eq!(code_point_slice(s, 12..12), Some(""));
        assert_eq!(code_point_slice(s, 3..3), Some(""));
        assert_eq!(code_point_slice(s, 0..13), None, "past the end");
        assert_eq!(code_point_slice(s, 13..13), None);
        assert_eq!(
            code_point_slice(s, std::ops::Range { start: 4, end: 2 }),
            None,
            "reversed"
        );
    }
}
