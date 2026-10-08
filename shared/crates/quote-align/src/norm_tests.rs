// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn n(s: &str) -> String {
    norm_v0(s).to_string()
}

/// The cases of `normalise_for_match_folds`
/// (`svrn/crates/sovereign-core/src/quote_verification.rs:490`), verbatim:
/// on text NFC leaves alone and with no line-end hyphen, norm_v0 IS that
/// fold. When `verify_quotes` converges onto this crate (wave 2), its own
/// test is the second witness.
#[test]
fn norm_v0_is_normalise_for_match_on_its_own_cases() {
    assert_eq!(n("a  b\t\nc"), "a b c");
    assert_eq!(n("  leading and trailing  "), "leading and trailing");
    assert_eq!(n(""), "");
    assert_eq!(
        n("Mrs Verloc\u{2019}s \u{201C}gaze\u{201D} \u{2014} steady"),
        "Mrs Verloc's \"gaze\" - steady"
    );
    assert_eq!(n("**bold** and _italic_"), "bold and italic");
    assert_eq!(n("wait\u{2026} what"), "wait... what");
}

#[test]
fn a_dropped_marker_does_not_split_a_whitespace_run() {
    assert_eq!(n("a _ b"), "a b");
    assert_eq!(n("a\u{2018}b\u{02BC}c\u{2013}d"), "a'b'c-d");
}

#[test]
fn nfc_comes_first_and_case_is_kept() {
    // e + COMBINING ACUTE composes; the composed é maps back to both inputs.
    let t = norm_v0("Caf\u{0065}\u{0301} Noir");
    assert_eq!(t.to_string(), "Caf\u{00E9} Noir");
    assert_eq!(t.origin(3..4), 3..5, "é came from two code points");
    assert_eq!(t.origin(5..9), 6..10);
    assert_eq!(n("The"), "The", "case is not folded");
}

#[test]
fn a_composing_starter_stays_in_its_cluster() {
    // Hangul L + V + T compose to one syllable from three starters.
    let t = norm_v0("\u{1100}\u{1161}\u{11A8}x");
    assert_eq!(t.to_string(), "\u{AC01}x");
    assert_eq!(t.origin(0..1), 0..3);
    assert_eq!(t.origin(1..2), 3..4);
}

#[test]
fn a_word_hyphenated_across_a_line_end_joins() {
    let t = norm_v0("the co-\noperation of  the parties");
    assert_eq!(t.to_string(), "the cooperation of the parties");
    // "cooperation" maps back over the hyphen and the line break.
    assert_eq!(t.origin(4..15), 4..17);
    assert_eq!(n("co-  \r\n  operation"), "cooperation");
    assert_eq!(n("co\u{00AD}\noperation"), "cooperation", "a soft hyphen");
}

#[test]
fn a_hyphen_that_is_not_a_split_word_stays() {
    assert_eq!(
        n("well-\nKnown"),
        "well- Known",
        "a capital starts a new word"
    );
    assert_eq!(n("1990-\n2000"), "1990- 2000", "digits are not a word");
    assert_eq!(
        n("word\u{2014}\nword"),
        "word- word",
        "a folded dash is no hyphen"
    );
    assert_eq!(n("co- operation"), "co- operation", "no line break");
}

#[test]
fn origins_cover_folds_runs_and_points() {
    let t = norm_v0("  wait\u{2026}  what  ");
    assert_eq!(t.to_string(), "wait... what");
    assert_eq!(t.origin(4..7), 6..7, "three dots, one ellipsis");
    assert_eq!(t.origin(7..8), 7..9, "one space, the whole run");
    assert_eq!(t.origin(8..12), 9..13);
    assert_eq!(t.origin(0..0), 2..2, "an empty range is a point");
    assert_eq!(t.origin(12..12), 13..13, "the end point");
    assert_eq!(norm_v0("").origin(0..0), 0..0);
}
