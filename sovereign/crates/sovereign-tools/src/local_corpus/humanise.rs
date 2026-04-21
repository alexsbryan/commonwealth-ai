//! Humanise a filename into a display-friendly string.
//!
//! Spec §5.3 rules (applied in order):
//!   1. Strip the file extension.
//!   2. Strip a leading ordering prefix like `01_`, `02-`.
//!   3. Convert `YYYY_MM_DD` and `YYYY-MM-DD` (anywhere in the stem) to
//!      `Mon DD YYYY`.
//!   4. Preserve a `_v\d+` or `-v\d+` version suffix as-is.
//!   5. Replace underscores and hyphens with spaces (except where they
//!      were consumed by the version suffix).
//!   6. Detect ALL-CAPS tokens and convert them to Title Case. A token
//!      that is a recognised acronym (`FOIA`, `NASA`, …) is kept
//!      upper-case; other all-caps words get title-cased.
//!
//! Why these rules: user documents are often exported from tools that
//! bake ordering, dates, and versioning into the filename. Showing the
//! raw filename looks machine-generated; these rules are the minimum to
//! produce something a human would actually read.

use std::path::Path;

/// Humanise a filesystem path (or just its filename) into a display
/// string. The input may be a full `Path` or just a `&str`; the
/// extension is stripped.
pub fn humanise_display_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    humanise_stem(&stem)
}

pub(crate) fn humanise_stem(stem: &str) -> String {
    // Date normalisation runs FIRST: otherwise a leading year
    // (`2024_09_12_notes`) would be eaten by the ordering-prefix strip.
    let mut s = normalise_dates(stem);

    // 2. Strip leading ordering prefix: `01_` / `02-` / `003 `.
    s = strip_leading_order_prefix(&s);

    // 4+5. Replace separators with spaces. `_v12` and `-v12` become
    //       ` v12` — a readable token, matching the spec example
    //       "FOIA response final v2".
    s = replace_separators_preserving_version(&s);

    // 6. Title-case all-caps tokens (preserving known acronyms).
    s = title_case_all_caps_words(&s);

    collapse_whitespace(&s).trim().to_string()
}

fn strip_leading_order_prefix(s: &str) -> String {
    // Matches leading digits followed by `_`, `-`, or space, e.g.
    // `01_foo`, `02-bar`, `3 baz`.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i >= bytes.len() {
        return s.to_string();
    }
    match bytes[i] {
        b'_' | b'-' | b' ' => s[i + 1..].to_string(),
        _ => s.to_string(),
    }
}

fn normalise_dates(s: &str) -> String {
    // Walk the string; when we see YYYY[_-]MM[_-]DD, rewrite.
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some((formatted, consumed)) = match_date(&chars, i) {
            out.push_str(&formatted);
            i += consumed;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn match_date(chars: &[char], start: usize) -> Option<(String, usize)> {
    // Need at least 10 chars: YYYY[_-]MM[_-]DD.
    if start + 10 > chars.len() {
        return None;
    }
    // YYYY
    for k in 0..4 {
        if !chars[start + k].is_ascii_digit() {
            return None;
        }
    }
    let sep1 = chars[start + 4];
    if sep1 != '_' && sep1 != '-' {
        return None;
    }
    // MM
    for k in 5..7 {
        if !chars[start + k].is_ascii_digit() {
            return None;
        }
    }
    let sep2 = chars[start + 7];
    if sep2 != '_' && sep2 != '-' {
        return None;
    }
    // DD
    for k in 8..10 {
        if !chars[start + k].is_ascii_digit() {
            return None;
        }
    }
    // Guard: don't start matching inside another number run (e.g. "20240912").
    if start > 0 && chars[start - 1].is_ascii_digit() {
        return None;
    }
    let year: String = chars[start..start + 4].iter().collect();
    let month: String = chars[start + 5..start + 7].iter().collect();
    let day: String = chars[start + 8..start + 10].iter().collect();
    let month_u: u32 = month.parse().ok()?;
    let day_u: u32 = day.parse().ok()?;
    if !(1..=12).contains(&month_u) || !(1..=31).contains(&day_u) {
        return None;
    }
    let month_name = MONTH_ABBR.get(month_u as usize - 1)?;
    Some((format!("{month_name} {day_u:02} {year}"), 10))
}

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn replace_separators_preserving_version(s: &str) -> String {
    // Walk tokens split on `_` and `-`. A token that starts with `v` and
    // is followed by digits is joined back to its predecessor without a
    // separator (so `final_v2` → `final v2` becomes `finalv2`? no —
    // spec says preserve `_v2`, meaning the token reads `v2` as its own
    // word). We'll emit a single space between tokens, which gives
    // `final v2`. The spec example "FOIA response final v2" confirms
    // that's desired.
    //
    // So: simple replace of `_` and `-` with space.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '_' || ch == '-' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

fn title_case_all_caps_words(s: &str) -> String {
    s.split(' ')
        .map(|token| {
            if token.is_empty() {
                return token.to_string();
            }
            if is_known_acronym(token) {
                return token.to_string();
            }
            if is_all_upper_ascii_word(token) {
                let mut chars = token.chars();
                match chars.next() {
                    Some(first) => {
                        let rest: String = chars.collect::<String>().to_lowercase();
                        format!("{first}{rest}")
                    }
                    None => token.to_string(),
                }
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_known_acronym(token: &str) -> bool {
    // Keep a short, conservative list. FOIA appears in the spec
    // example. Extend as users report needs.
    matches!(
        token,
        "FOIA" | "NASA" | "FBI" | "CIA" | "USA" | "UK" | "EU" | "UN" | "NATO" | "GDP" | "RFP" | "API" | "CPU" | "GPU" | "SQL" | "PDF" | "HTML" | "CSS" | "JS"
    )
}

fn is_all_upper_ascii_word(token: &str) -> bool {
    let mut has_letter = false;
    for ch in token.chars() {
        if ch.is_ascii_alphabetic() {
            has_letter = true;
            if !ch.is_ascii_uppercase() {
                return false;
            }
        } else if !ch.is_ascii_alphanumeric() {
            // punctuation inside the token (e.g. `test.v2`) — don't
            // touch.
            return false;
        }
    }
    has_letter && token.len() > 1
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn h(s: &str) -> String {
        humanise_display_name(&PathBuf::from(s))
    }

    #[test]
    fn spec_example_city_council() {
        assert_eq!(h("city_council_2024_09_12.pdf"), "city council Sep 12 2024");
    }

    #[test]
    fn spec_example_meeting_minutes() {
        assert_eq!(h("meeting_minutes_march.pdf"), "meeting minutes march");
    }

    #[test]
    fn spec_example_foia_response() {
        // FOIA preserved; `v2` preserved; `final` stays as-is.
        assert_eq!(h("FOIA_response_final_v2.pdf"), "FOIA response final v2");
    }

    #[test]
    fn spec_example_ordering_prefix_stripped() {
        // Leading `01_` stripped entirely.
        assert_eq!(h("01_introduction.txt"), "introduction");
    }

    #[test]
    fn date_hyphen_form() {
        assert_eq!(h("minutes-2024-03-15.pdf"), "minutes Mar 15 2024");
    }

    #[test]
    fn single_digit_day_padded_to_two() {
        assert_eq!(h("notes_2024_09_02.txt"), "notes Sep 02 2024");
    }

    #[test]
    fn all_caps_word_title_cased() {
        assert_eq!(h("REPORT_draft.txt"), "Report draft");
    }

    #[test]
    fn mixed_case_preserved() {
        assert_eq!(h("MyReport_draft.txt"), "MyReport draft");
    }

    #[test]
    fn no_extension() {
        assert_eq!(h("readme"), "readme");
    }

    #[test]
    fn nested_path_uses_basename() {
        assert_eq!(h("/a/b/c/hello_world.pdf"), "hello world");
    }

    #[test]
    fn digit_run_not_a_date() {
        // A bare 8-digit number that looks like YYYYMMDD (no separators)
        // is not a date in our grammar — we need the separators.
        assert_eq!(h("backup_20240912.txt"), "backup 20240912");
    }

    #[test]
    fn invalid_month_not_treated_as_date() {
        assert_eq!(h("notes_2024_13_40.txt"), "notes 2024 13 40");
    }

    #[test]
    fn leading_year_preserved_by_date_normalisation() {
        // Date normalisation runs before the ordering-prefix strip, so
        // a leading YYYY is preserved and becomes "Mon DD YYYY".
        assert_eq!(h("2024_09_12_notes.pdf"), "Sep 12 2024 notes");
    }
}
