// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::code_point_slice;

fn cfg() -> AlignConfig {
    AlignConfig::shipped().expect("the shipped align.toml loads")
}

const RIVER: &str = "The river keeps its own counsel, and the town keeps its own.";

fn one(quote: &str, text: &str) -> QuoteAlignment {
    let found = align(quote, &[text], &cfg());
    assert_eq!(found.alignments.len(), 1, "{quote:?}: {found:?}");
    found.alignments.into_iter().next().unwrap()
}

fn edits(quote: &str, text: &str, a: &QuoteAlignment) -> Vec<(QuoteEditKind, String, String)> {
    a.edits
        .iter()
        .map(|e| {
            (
                e.kind,
                code_point_slice(quote, e.quote.clone())
                    .unwrap()
                    .to_string(),
                code_point_slice(text, e.source.clone())
                    .unwrap()
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn a_verbatim_quote_binds_exactly_with_no_edits() {
    let a = one("the town keeps its own", RIVER);
    assert_eq!(
        code_point_slice(RIVER, a.source.clone()),
        Some("the town keeps its own")
    );
    assert!(a.edits.is_empty());
    assert_eq!(a.coverage, 1.0);
}

#[test]
fn one_changed_word_is_one_substitution_at_the_right_ranges() {
    let quote = "The river keeps her own counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        code_point_slice(RIVER, a.source.clone()),
        Some("The river keeps its own counsel")
    );
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Substituted, "her".into(), "its".into())]
    );
    assert_eq!(a.edits[0].quote, 16..19);
    assert_eq!(a.edits[0].source, 16..19);
}

#[test]
fn an_extra_word_is_added_and_a_dropped_one_omitted() {
    let quote = "The river keeps all its own counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Added, "all".into(), String::new())]
    );
    assert_eq!(
        a.edits[0].source,
        15..15,
        "the point after the last aligned word"
    );

    let quote = "The river keeps its counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Omitted, String::new(), "own".into())]
    );
}

#[test]
fn a_word_changed_at_either_edge_is_still_a_substitution() {
    let a = one("A river keeps its own counsel", RIVER);
    assert_eq!(a.edits[0].kind, QuoteEditKind::Substituted);
    assert_eq!(a.source.start, 0, "the span keeps the source's first word");
    let quote = "The river keeps its own secrets";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(
            QuoteEditKind::Substituted,
            "secrets".into(),
            "counsel".into()
        )]
    );
}

#[test]
fn case_is_reported_and_the_client_judges_it() {
    let quote = "the river keeps its own counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Substituted, "the".into(), "The".into())]
    );
}

#[test]
fn an_ellipsis_elides_and_brackets_are_editorial() {
    let quote = "The river \u{2026} counsel, and the town keeps its own";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(
            QuoteEditKind::Elided,
            "\u{2026}".into(),
            "keeps its own".into()
        )]
    );
    assert_eq!(a.coverage, 1.0, "an honest elision costs no coverage");

    let quote = "[t]he river keeps its own counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Bracketed, "[t]he".into(), "The".into())]
    );
}

#[test]
fn ranges_are_code_points_of_the_inputs_as_given() {
    let text = "Caf\u{0065}\u{0301} \u{201C}noir\u{201D} keeps its own co-\noperation entirely";
    let quote = "\"noir\" keeps its own cooperation";
    let a = one(quote, text);
    assert!(a.edits.is_empty(), "{:?}", a.edits);
    assert_eq!(
        code_point_slice(text, a.source.clone()),
        Some("\u{201C}noir\u{201D} keeps its own co-\noperation")
    );
}

#[test]
fn the_best_text_wins_and_unrelated_quotes_find_nothing() {
    let other = "A different river keeps nothing to itself at all.";
    let found = align("the town keeps its own", &[other, RIVER], &cfg());
    assert_eq!(found.alignments.len(), 1);
    assert_eq!(found.alignments[0].text, 1);

    let none = align(
        "an entirely unrelated sentence about tax law",
        &[RIVER],
        &cfg(),
    );
    assert!(none.alignments.is_empty());
    assert_eq!(none.anchors, 0, "no 3-gram seeded anything");
}

#[test]
fn the_floor_drops_a_poor_alignment_and_says_so() {
    let found = align("The river keeps nothing whatsoever back", &[RIVER], &cfg());
    assert!(found.alignments.is_empty(), "{found:?}");
    assert_eq!(found.below_floor, 1);
}

#[test]
fn every_occurrence_is_its_own_alignment() {
    let text = "keeps its own counsel. Later, it keeps its own counsel again.";
    let found = align("keeps its own counsel", &[text], &cfg());
    let spans: Vec<_> = found.alignments.iter().map(|a| a.source.clone()).collect();
    assert_eq!(spans, [0..21, 33..54]);
}
