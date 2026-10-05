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
//!
//! [`metadata_date`] is the second reader, for corpora whose documents carry
//! their own date field (mail, front matter): the recipe names the field in
//! `change.document.date` and resolution stamps the result on each claim as
//! `document_date`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};

/// A document's own date — the value of the metadata field the recipe's
/// `change.document.date` names — as ISO 8601, or `None`.
///
/// Unlike [`section_date`] this reads a field whose whole job is to be a
/// date, so it parses the two shapes such fields carry: RFC 2822 (a mail
/// `Date:` header) and ISO 8601 (RFC 3339 date-times, calendar dates, and
/// date-times with no offset). An instant with an offset is written in UTC
/// (`2001-05-14T23:39:00Z`), so the supersession fold's string order is
/// time order across zones; a bare date stays a date; a date-time with no
/// offset keeps its wall-clock reading and invents no zone. Anything else is
/// `None` — the caller counts it, never defaults it.
pub fn metadata_date(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let utc = |d: DateTime<chrono::FixedOffset>| {
        d.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    };
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(utc(d));
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(utc(d));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.format("%Y-%m-%d").to_string());
    }
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
}

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
    fn metadata_date_reads_rfc2822_and_iso8601() {
        // An Enron-shaped mail header, comment and all, lands in UTC.
        assert_eq!(
            metadata_date("Mon, 14 May 2001 16:39:00 -0700 (PDT)").as_deref(),
            Some("2001-05-14T23:39:00Z")
        );
        assert_eq!(
            metadata_date("Tue, 15 May 2001 09:05:00 +0000").as_deref(),
            Some("2001-05-15T09:05:00Z")
        );
        assert_eq!(
            metadata_date("2025-03-14T10:00:00+02:00").as_deref(),
            Some("2025-03-14T08:00:00Z")
        );
        assert_eq!(metadata_date(" 2025-03-14 ").as_deref(), Some("2025-03-14"));
        // No offset written, none invented.
        assert_eq!(
            metadata_date("2025-03-14T10:00:00").as_deref(),
            Some("2025-03-14T10:00:00")
        );
    }

    #[test]
    fn metadata_date_refuses_what_is_not_a_date() {
        for raw in [
            "",
            "   ",
            "yesterday",
            "14/3/25",
            "2025-13-01",
            "Mon, 32 May 2001",
        ] {
            assert_eq!(metadata_date(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn section_date_takes_the_first_date_in_a_range() {
        assert_eq!(
            section_date("Valid 2024-01-01 to 2024-06-30"),
            NaiveDate::from_ymd_opt(2024, 1, 1)
        );
    }
}
