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
    let found = align("the town keeps its own", &[RIVER], &cfg());
    let a = &found.alignments[0];
    assert_eq!(
        code_point_slice(RIVER, a.source.clone()),
        Some("the town keeps its own")
    );
    assert!(a.edits.is_empty());
    assert_eq!(a.coverage, 1.0);
    // "The river keeps its own" is the near passage, one word away: it is
    // reported after the exact one whatever the quotation's case.
    assert_eq!(
        found.alignments[1..]
            .iter()
            .map(|n| code_point_slice(RIVER, n.source.clone()))
            .collect::<Vec<_>>(),
        [Some("The river keeps its own")]
    );
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

/// One changed word in the middle of five sits in every 3-gram; the seed
/// backs off to 2-grams and the substitution is found.
#[test]
fn a_short_quotation_with_its_middle_word_changed_seeds_through_shorter_grams() {
    let quote = "river keeps her own counsel";
    let a = one(quote, RIVER);
    assert_eq!(
        edits(quote, RIVER, &a),
        [(QuoteEditKind::Substituted, "her".into(), "its".into())]
    );
    assert_eq!(a.coverage, 0.8);
}

/// A quotation whose first letter's case is the only difference seeds and
/// covers ("Counsel" for "counsel"); any other case change seeds but does
/// not cover, so the floor drops it and says so.
#[test]
fn a_quotation_differing_only_in_case_is_found() {
    let a = one("Counsel", RIVER);
    assert_eq!(
        edits("Counsel", RIVER, &a),
        [(
            QuoteEditKind::Substituted,
            "Counsel".into(),
            "counsel".into()
        )]
    );
    assert_eq!(a.coverage, 1.0);

    let found = align("OWN COUNSEL", &[RIVER], &cfg());
    assert!(found.anchors > 0, "it seeds, case folded: {found:?}");
    assert!(found.alignments.is_empty(), "{found:?}");
    assert_eq!(found.below_floor, 1);
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
    assert_eq!(found.alignments[0].text, 1);
    assert!(found.alignments[0].edits.is_empty());
    assert!(
        found.alignments.iter().all(|a| a.text == 1),
        "the other text holds no passage near enough: {found:?}"
    );

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

// PAL1 (OICP v0.6 §2.18): the quotations of the holdout essay's dry run
// (2026-10-18), against the sentences of its library they met there.
const MALTHUS: &str = "It has been said that the great number of unmarried persons is to be rather an argument against their population. I should rather draw a contrary inference and consider it an argument of their fullness, though this inference is not certain, because there are many thinly inhabited states that are yet stationary in their population. To speak, therefore, correctly, perhaps it may be said that the number of unmarried persons in proportion to the whole number may be a criterion of the real increase in their population.";
const MILL: &str = "The ordinary progress of a society which increases in wealth is at all times tending to augment the incomes of landlords; to give them both a greater amount and a greater proportion of the wealth of the community, independently of any trouble or outlay incurred by themselves. They grow richer, as it were, in their sleep, without working, risking, or economizing. What claim have they, on the general principle of social justice, to this accession of riches?";
const ARISTOTLE: &str = "But it is absurd to make wives common; and it is absurd to draw a comparison from the brute creation, and say, that the same principle should regulate the connection of a man and a woman which regulates theirs amongst whom there is no family association.";
const LOCKE: &str = "For this labour being the unquestionable property of the labourer, no man but he can have a right to what that is once joined to, at least where there is enough, and as good, left in common for others.";
const SOULS: &str = "Away back in the thirties the melody of these slave songs stirred the nation, but the songs were soon half forgotten.";
const HAMLET: &str = "To be, or not to be, that is the question: Whether 'tis nobler in the mind to suffer The slings and arrows of outrageous fortune, Or to take arms against a sea of troubles, And by opposing end them.";

#[test]
fn a_quotation_closing_with_a_stop_aligns_where_its_words_stand() {
    let quote = "in their sleep.";
    let found = align(quote, &[MALTHUS, MILL], &cfg());
    let best = &found.alignments[0];
    assert_eq!(best.text, 1, "Mill's words, not Malthus's: {found:?}");
    assert_eq!(
        code_point_slice(MILL, best.source.clone()),
        Some("in their sleep,")
    );
    assert_eq!(
        edits(quote, MILL, best),
        [(QuoteEditKind::Substituted, ".".into(), ",".into())]
    );
    assert_eq!(best.coverage, 1.0, "a mark is not a word");
    assert!(
        found.alignments.iter().all(|a| a.text == 1),
        "no span with the quotation's word changed is kept: {found:?}"
    );
}

#[test]
fn a_quotation_in_no_text_is_not_aligned_at_its_one_rare_word_changed() {
    let texts = [MALTHUS, MILL, ARISTOTLE, LOCKE];
    for quote in ["there is no alternative.", "there is no alternative"] {
        let found = align(quote, &texts, &cfg());
        assert!(found.alignments.is_empty(), "{quote:?}: {found:?}");
    }
}

#[test]
fn a_dropped_comma_is_still_reported_and_a_changed_common_word_still_aligns() {
    let quote = "enough and as good";
    let a = one(quote, LOCKE);
    assert_eq!(
        code_point_slice(LOCKE, a.source.clone()),
        Some("enough, and as good")
    );
    assert_eq!(
        edits(quote, LOCKE, &a),
        [(QuoteEditKind::Omitted, String::new(), ",".into())]
    );

    let quote = "melody of the slave songs,";
    let a = one(quote, SOULS);
    assert_eq!(
        edits(quote, SOULS, &a),
        [
            (QuoteEditKind::Substituted, "the".into(), "these".into()),
            (QuoteEditKind::Added, ",".into(), String::new())
        ]
    );
}

#[test]
fn a_long_quotation_with_its_rare_word_changed_still_aligns() {
    let quote = "To be, or not to be, that is the problem";
    let a = one(quote, HAMLET);
    assert_eq!(
        edits(quote, HAMLET, &a),
        [(
            QuoteEditKind::Substituted,
            "problem".into(),
            "question".into()
        )]
    );
}
