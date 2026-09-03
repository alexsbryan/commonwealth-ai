// SPDX-License-Identifier: AGPL-3.0-or-later
//! Axis 4's clock — reading a date out of what a corpus already writes.
//!
//! `change.clock` defaults to `document_date` and
//! `ONTOLOGY_PRIMITIVES.md` §2 axis 4 says why it is DERIVED rather than
//! declared: "document dates present means document_date". Something has to
//! find them, and in the corpora this serves — minutes, decisions, dated
//! articles, catalogue entries — the date is in the section heading the
//! author already wrote:
//!
//! ```text
//! ## Decision 2025-03-14 — overnight guests
//! ## 2024-11 House meeting
//! ```
//!
//! [`section_date`] is that reader, and it is deliberately narrow: it
//! recognises ISO-8601 calendar dates and year-months, anywhere in the
//! title, and nothing else. A looser parser ("March 2025", "14/3/25") buys
//! recall in exchange for the one failure this must not have — reading a
//! number that is not a date and folding a rule that is still in force. A
//! title it cannot read yields `None`, which the fold reads as "no clock
//! for this rule" and therefore "nothing supersedes it".

use chrono::NaiveDate;

/// The first ISO-8601 date in a section title, or `None`.
///
/// Accepts `YYYY-MM-DD` and `YYYY-MM` (read as the first of that month, so
/// two rules in the same month are contemporaneous rather than ordered by
/// an invented day). Requires four-digit years and zero-padded
/// month/day — the shape `chrono` itself round-trips — so a bare `2025` or
/// a section number like `4-2` is not mistaken for a date.
///
/// Returns the FIRST match: a heading that names a range
/// (`"2024-01-01 to 2024-06-30"`) is read at its start, which is when the
/// document speaks.
pub fn section_date(title: &str) -> Option<NaiveDate> {
    let bytes = title.as_bytes();
    let mut i = 0usize;
    while i + 7 <= bytes.len() {
        // A candidate starts at a digit that is not preceded by one, so
        // "12025-01" is not read as "2025-01".
        if !bytes[i].is_ascii_digit() || (i > 0 && bytes[i - 1].is_ascii_digit()) {
            i += 1;
            continue;
        }
        if let Some(d) = parse_at(&bytes[i..]) {
            return Some(d);
        }
        i += 1;
    }
    None
}

/// Parse `YYYY-MM-DD` or `YYYY-MM` at the start of `s`, rejecting a
/// year-month that is actually the head of a longer digit run.
fn parse_at(s: &[u8]) -> Option<NaiveDate> {
    if s.len() < 7 {
        return None;
    }
    if !(s[0..4].iter().all(u8::is_ascii_digit)
        && s[4] == b'-'
        && s[5..7].iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    let year: i32 = std::str::from_utf8(&s[0..4]).ok()?.parse().ok()?;
    let month: u32 = std::str::from_utf8(&s[5..7]).ok()?.parse().ok()?;

    // Full calendar date when a `-DD` follows and is not itself the head of
    // a longer run of digits.
    if s.len() >= 10
        && s[7] == b'-'
        && s[8..10].iter().all(u8::is_ascii_digit)
        && !s.get(10).is_some_and(|c| c.is_ascii_digit())
    {
        let day: u32 = std::str::from_utf8(&s[8..10]).ok()?.parse().ok()?;
        return NaiveDate::from_ymd_opt(year, month, day);
    }
    // Year-month, only when nothing more of the date follows.
    if s.get(7).is_some_and(|c| c.is_ascii_digit() || *c == b'-') {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_date_parses_iso() {
        assert_eq!(
            section_date("## Decision 2025-03-14 — overnight guests"),
            NaiveDate::from_ymd_opt(2025, 3, 14)
        );
        assert_eq!(
            section_date("2024-11 House meeting"),
            NaiveDate::from_ymd_opt(2024, 11, 1)
        );
        assert_eq!(
            section_date("Article IV — quiet hours"),
            None,
            "a heading with no date has no clock"
        );
    }

    #[test]
    fn section_date_refuses_things_that_are_not_dates() {
        // Section numbering, not a date.
        assert_eq!(section_date("4-2 Parking"), None);
        // A bare year is not a document date.
        assert_eq!(section_date("Minutes 2025"), None);
        // Out-of-range month.
        assert_eq!(section_date("2025-13-01 nonsense"), None);
        // A longer digit run must not be sliced into a date.
        assert_eq!(section_date("ref 12025-01-02"), None);
    }

    #[test]
    fn section_date_takes_the_first_date_in_a_range() {
        assert_eq!(
            section_date("Valid 2024-01-01 to 2024-06-30"),
            NaiveDate::from_ymd_opt(2024, 1, 1)
        );
    }
}
