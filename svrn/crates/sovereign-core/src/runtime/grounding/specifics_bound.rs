// SPDX-License-Identifier: AGPL-3.0-or-later
//! A figure's or a quantity's bound (§2.15): "more than 50 years" asserts a
//! range, not the point 50, so a passage reading "more than a hundred years"
//! does not state it otherwise. Read by the words right before the number,
//! the longest phrase first ("no more than" is at most, not more than). A
//! date takes no bound ("over 1750" is not a reading anyone writes), and an
//! approximation ("about", "nearly") is none: it widens a point by an amount
//! nobody states.
//!
//! Two specifics differ under a bound only when no value meets both
//! ([`ranges_meet`]): "fewer than 50 years" against "more than a hundred" is
//! a contradiction, "more than 50" against it is not, and "4,800 pins"
//! against "upwards of forty-eight thousand" still is. The operator chose
//! this literal reading on 2026-10-15 (zoracite v1-claims): a weaker bound in
//! the right direction is true, and verify never answers a true claim
//! "your source says otherwise".

use super::{plain, words_with_offsets, Specific, SpecificKind};

/// Which side of its number a bounded specific's value lies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FigureBound {
    /// Strictly more: "more than", "over", "upwards of".
    Above,
    /// The number or more: "at least", "no fewer than".
    AtLeast,
    /// Strictly less: "less than", "fewer than", "under".
    Below,
    /// The number or less: "at most", "up to", "no more than".
    AtMost,
}

/// The phrases that bound a number, as lower-case words.
const BOUNDS: &[(&str, FigureBound)] = &[
    ("more than", FigureBound::Above),
    ("over", FigureBound::Above),
    ("above", FigureBound::Above),
    ("upwards of", FigureBound::Above),
    ("upward of", FigureBound::Above),
    ("in excess of", FigureBound::Above),
    ("greater than", FigureBound::Above),
    ("at least", FigureBound::AtLeast),
    ("no fewer than", FigureBound::AtLeast),
    ("no less than", FigureBound::AtLeast),
    ("not less than", FigureBound::AtLeast),
    ("less than", FigureBound::Below),
    ("fewer than", FigureBound::Below),
    ("under", FigureBound::Below),
    ("below", FigureBound::Below),
    ("at most", FigureBound::AtMost),
    ("up to", FigureBound::AtMost),
    ("no more than", FigureBound::AtMost),
    ("not more than", FigureBound::AtMost),
];

/// The bound the words of `sentence` before code point `start` put on the
/// number there, if any.
fn bound_before(sentence: &str, start: usize) -> Option<FigureBound> {
    let head: String = sentence.chars().take(start).collect();
    let words: Vec<String> = words_with_offsets(&head)
        .into_iter()
        .map(|(w, _)| plain(Some(w)))
        .collect();
    (1..=3.min(words.len())).rev().find_map(|k| {
        let phrase = words[words.len() - k..].join(" ");
        BOUNDS
            .iter()
            .find(|(p, _)| *p == phrase)
            .map(|&(_, bound)| bound)
    })
}

/// Sets each figure's and quantity's bound from the words before it.
pub(super) fn read_bounds(sentence: &str, specifics: &mut [Specific]) {
    for s in specifics
        .iter_mut()
        .filter(|s| matches!(s.kind, SpecificKind::Figure | SpecificKind::Quantity))
    {
        s.bound = bound_before(sentence, s.range.start);
    }
}

/// One end of a range: its value, and whether the value itself is inside.
type End = Option<(f64, bool)>;

/// The values `s` admits, as (low, high) ends; `None` when its value is not a
/// number this reads.
fn range_of(s: &Specific) -> Option<(End, End)> {
    let v: f64 = s.value.as_deref()?.parse().ok()?;
    Some(match s.bound {
        None => (Some((v, true)), Some((v, true))),
        Some(FigureBound::Above) => (Some((v, false)), None),
        Some(FigureBound::AtLeast) => (Some((v, true)), None),
        Some(FigureBound::Below) => (None, Some((v, false))),
        Some(FigureBound::AtMost) => (None, Some((v, true))),
    })
}

/// Whether some value meets both `a` and `b`, either of them bounded; `None`
/// when one's value is not a number this reads, or neither is bounded (two
/// points meet only when equal, which their values already say).
pub(super) fn ranges_meet(a: &Specific, b: &Specific) -> Option<bool> {
    if a.bound.is_none() && b.bound.is_none() {
        return None;
    }
    let ((alo, ahi), (blo, bhi)) = (range_of(a)?, range_of(b)?);
    let higher_low = match (alo, blo) {
        (Some(x), Some(y)) if y.0 > x.0 || (y.0 == x.0 && !y.1) => Some(y),
        (Some(x), _) => Some(x),
        (None, y) => y,
    };
    let lower_high = match (ahi, bhi) {
        (Some(x), Some(y)) if y.0 < x.0 || (y.0 == x.0 && !y.1) => Some(y),
        (Some(x), _) => Some(x),
        (None, y) => y,
    };
    Some(match (higher_low, lower_high) {
        (Some((lo, lo_in)), Some((hi, hi_in))) => lo < hi || (lo == hi && lo_in && hi_in),
        _ => true,
    })
}
