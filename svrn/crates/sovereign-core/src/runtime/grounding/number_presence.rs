// SPDX-License-Identifier: AGPL-3.0-or-later
//! Numeric presence: a value that IS a number is found by its VALUE among the
//! numbers a passage holds, not by its alphanumeric pieces.
//!
//! The word path in [`super::value_present_in_chunks`] splits on every
//! non-alphanumeric character and drops pieces under two, so "3.2%" left
//! nothing (false red on a correct figure) and "4.12%" was looked for as
//! "12". Pinned port: `bench/.../flywheel/det_checks.rs::value_present`
//! carries the same functions; keep them in step.
//!
//! Magnitude only: a sign on the value is ignored, so "-3.2%" is found by "a
//! fall of 3.2 per cent". This is a veto on literal presence, not support.

/// Canonical form of every number in `s`: thousands commas dropped, leading
/// zeros and trailing fractional zeros trimmed ("3.20" == "3.2" == "03.2").
pub(super) fn numbers_in(s: &str) -> Vec<String> {
    scan(s).into_iter().map(|(_, _, c)| c).collect()
}

/// `Some(canonical)` when `value` is purely a number with optional sign,
/// currency sign and percent marker; `None` leaves it to the word path.
pub(super) fn numeric_value(value: &str) -> Option<String> {
    let lower = value.trim().to_lowercase();
    let s = lower.trim_start_matches(|c: char| {
        matches!(c, '+' | '-' | '\u{2212}' | '$' | '€' | '£') || c.is_whitespace()
    });
    let s = s.trim_end();
    let s = ["percent", "per cent", "per-cent", "%"]
        .iter()
        .find_map(|m| s.strip_suffix(m))
        .unwrap_or(s)
        .trim_end();
    match scan(s).as_slice() {
        [(0, end, canon)] if *end == s.len() => Some(canon.clone()),
        _ => None,
    }
}

/// `(start, end, canonical)` per number. A comma joins only before exactly
/// three digits; a dot only before a digit and once.
fn scan(s: &str) -> Vec<(usize, usize, String)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let (mut int, mut frac, mut dot) = (String::new(), String::new(), false);
        while i < b.len() {
            let c = b[i];
            let three = |j: usize| {
                b.len() >= j + 4
                    && b[j + 1..j + 4].iter().all(u8::is_ascii_digit)
                    && !b.get(j + 4).is_some_and(u8::is_ascii_digit)
            };
            if c.is_ascii_digit() {
                if dot {
                    frac.push(c as char)
                } else {
                    int.push(c as char)
                }
            } else if c == b',' && !dot && three(i) {
            } else if c == b'.' && !dot && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                dot = true;
            } else {
                break;
            }
            i += 1;
        }
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let int = if int.is_empty() { "0" } else { int };
        let canon = if frac.is_empty() {
            int.to_string()
        } else {
            format!("{int}.{frac}")
        };
        out.push((start, i, canon));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::value_present_in_chunks as present;

    fn one(t: &str) -> Vec<String> {
        vec![t.to_string()]
    }

    #[test]
    fn a_decimal_percent_is_found_by_its_value_not_its_pieces() {
        assert!(present("3.2%", &one("It grew 3.2 per cent.")));
        assert!(present("3.2%", &one("It grew 3.2%.")));
        assert!(present("3.2 percent", &one("up 3.20%")));
        assert!(present("$1,200", &one("cost 1200 dollars")));
        assert!(present("1200", &one("cost $1,200.00")));
    }

    #[test]
    fn a_number_is_not_found_by_a_fragment_of_it() {
        assert!(!present("4.12%", &one("only 12 here")));
        assert!(!present("4.12%", &one("room 412 here")));
        assert!(!present("3.2%", &one("3.25% and 13.2%")));
        assert!(!present("12", &one("room 412")));
    }

    #[test]
    fn a_non_numeric_value_keeps_the_word_path() {
        assert!(present("Karl Yundt", &one("karl yundt spoke")));
        assert!(present("3 million", &one("about 3 million people")));
    }
}
