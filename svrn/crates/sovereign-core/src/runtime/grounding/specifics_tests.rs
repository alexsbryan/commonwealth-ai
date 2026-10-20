// SPDX-License-Identifier: AGPL-3.0-or-later
//! `specifics.rs`'s tests: sentences, specifics by kind, citations and
//! locators left out, and what a passage states otherwise.

use super::*;

fn texts(s: &str) -> Vec<(SpecificKind, String)> {
    sentence_specifics(s)
        .into_iter()
        .map(|x| (x.kind, x.text))
        .collect()
}

/// `s`'s code points `r`.
fn at(s: &str, r: &Range<usize>) -> String {
    s.chars().skip(r.start).take(r.len()).collect()
}

#[test]
fn sentences_split_at_stops_before_an_opener_and_not_after_an_initial() {
    let t = "J. S. Mill wrote in 1859. It sold well!  \"Then\" it ended? no stop here";
    let got: Vec<String> = sentence_ranges(t).iter().map(|r| at(t, r)).collect();
    assert_eq!(
        got,
        vec![
            "J. S. Mill wrote in 1859.",
            "It sold well!",
            "\"Then\" it ended? no stop here",
        ]
    );
    assert!(sentence_ranges("  \n ").is_empty());
}

/// A locator's or a citation's abbreviation before a number does not end a
/// sentence, so a citation stays inside the claim it belongs to.
#[test]
fn a_citations_abbreviation_does_not_end_the_sentence() {
    let t = "It sold well (Okafor et al. 2019, p. 4). Then it ended (Vol. 2).";
    let got: Vec<String> = sentence_ranges(t).iter().map(|r| at(t, r)).collect();
    assert_eq!(
        got,
        vec![
            "It sold well (Okafor et al. 2019, p. 4).",
            "Then it ended (Vol. 2)."
        ]
    );
}

#[test]
fn numbers_split_into_figures_dates_and_quantities_beside_name_runs() {
    assert_eq!(
        texts("The survey by Adaeze Okafor covered 14 agreements in 2019."),
        vec![
            (SpecificKind::Name, "Adaeze Okafor".into()),
            (SpecificKind::Quantity, "14".into()),
            (SpecificKind::Date, "2019".into()),
        ]
    );
    assert_eq!(
        texts("Hamilton, Madison and Jay wrote; Hume's Treatise followed."),
        vec![
            (SpecificKind::Name, "Madison".into()),
            (SpecificKind::Name, "Jay".into()),
            (SpecificKind::Name, "Hume".into()),
            (SpecificKind::Name, "Treatise".into()),
        ]
    );
    assert!(texts("it rained on the river all week.").is_empty());
    let s =
        sentence_specifics("Costs rose 3.2% to $1,200, 7.5 percent of 31 of them, on 12 March.");
    let got: Vec<(SpecificKind, &str, Option<&str>)> = s
        .iter()
        .map(|x| (x.kind, x.text.as_str(), x.unit.as_deref()))
        .collect();
    assert_eq!(
        got,
        vec![
            (SpecificKind::Figure, "3.2%", Some("percent")),
            (SpecificKind::Figure, "$1,200", Some("$")),
            (SpecificKind::Figure, "7.5", Some("percent")),
            (SpecificKind::Figure, "31", None),
            (SpecificKind::Date, "12", None),
        ]
    );
    let q = sentence_specifics("The port recorded 412 sailings.");
    assert_eq!(q[0].unit.as_deref(), Some("sailing"), "{q:?}");
}

/// The two shapes measured as false flags on X1 (see the module doc): a bare
/// one-digit number, and a lone capitalised word opening the sentence. A name
/// of two words opening it still counts, at its own range.
#[test]
fn a_one_digit_number_and_a_capitalised_first_word_are_not_specifics() {
    assert_eq!(
        texts("Finally, Book 3 of the Treatise turns to morals."),
        vec![
            (SpecificKind::Name, "Book".into()),
            (SpecificKind::Name, "Treatise".into()),
        ]
    );
    assert_eq!(texts("Section 2's argument fails."), vec![]);
    let s = "\"Adam Smith read it in 1740.";
    let got = sentence_specifics(s);
    assert_eq!(got[0].text, "Adam Smith");
    assert_eq!(got[0].range, 1..11, "the range skips the opening quote");
    assert_eq!(got[1].text, "1740");
    assert_eq!(
        texts("They sent 3 ships and 5% of the crew."),
        vec![
            (SpecificKind::Quantity, "3".into()),
            (SpecificKind::Figure, "5%".into()),
        ]
    );
}

#[test]
fn a_specifics_range_names_it_in_the_sentence() {
    let s = "Ce café, dit Émile Zola en 1898, coûtait €12 (soit 4.5% de plus).";
    for x in sentence_specifics(s) {
        let there = at(s, &x.range);
        assert!(
            there.starts_with(x.text.split(' ').next().unwrap()),
            "{x:?}"
        );
        assert_eq!(there.chars().count(), x.range.len());
        if x.kind != SpecificKind::Name {
            assert_eq!(there, x.text, "a number's text is its range");
        }
    }
    assert_eq!(
        texts(s),
        vec![
            (SpecificKind::Name, "Émile Zola".into()),
            (SpecificKind::Date, "1898".into()),
            (SpecificKind::Figure, "€12".into()),
            (SpecificKind::Figure, "4.5%".into()),
        ]
    );
}

/// Failing input (platform row PC1): a citation's year made a figure. Nothing
/// inside a citation or a locator is a specific, name or number.
#[test]
fn nothing_inside_a_citation_or_a_locator_is_a_specific() {
    for (s, cited) in [
        (
            "The port recorded 412 sailings in 2019 (Okafor 2019).",
            "(Okafor 2019)",
        ),
        (
            "Keepers were paid 300 shillings a year (page 14).",
            "(page 14)",
        ),
        (
            "The basin held 14 treaties (Okafor, 2019, p. 4).",
            "(Okafor, 2019, p. 4)",
        ),
        (
            "The basin held 14 treaties (Okafor and Lee 2019; Ruiz 2020a).",
            "(Okafor and Lee 2019; Ruiz 2020a)",
        ),
        ("The basin held 14 treaties [@okafor2019].", "[@okafor2019]"),
        ("The basin held 14 treaties [12, 13].", "[12, 13]"),
        ("The basin held 14 treaties (p. 3).", "(p. 3)"),
        ("The basin held 14 treaties (§ 4).", "(§ 4)"),
        ("The basin held 14 treaties (chapter 22).", "(chapter 22)"),
    ] {
        let byte = s.find(cited).unwrap();
        let a = s[..byte].chars().count();
        let b = a + cited.chars().count();
        let got = sentence_specifics(s);
        assert!(
            got.iter().all(|x| x.range.end <= a || b <= x.range.start),
            "a specific inside `{cited}`: {got:?}"
        );
        assert!(
            got.iter().any(|x| x.kind == SpecificKind::Quantity),
            "the claim's own number stays: {got:?}"
        );
    }
}

/// What a model judging a claim is shown: the claim less what
/// [`cited_ranges`] recognises, the whitespace at each cut tidied, and
/// parentheses that are no citation kept with their figure.
#[test]
fn without_citations_cuts_what_the_specifics_leave_out_and_nothing_else() {
    for (s, want, n) in [
        (
            "The port recorded 412 sailings in 2019 (Okafor 2019).",
            "The port recorded 412 sailings in 2019.",
            1,
        ),
        (
            "The port recorded 412 sailings in 2019 [@okafor2019].",
            "The port recorded 412 sailings in 2019.",
            1,
        ),
        (
            "The basin held 14 treaties (p. 12), [3] by 1990 [12, 13].",
            "The basin held 14 treaties, by 1990.",
            3,
        ),
        (
            "[3] The basin held 14 treaties (see Okafor 2019; Ruiz 2020a).",
            "The basin held 14 treaties.",
            2,
        ),
        (
            "Late sailings rose to 7.5 percent (against 6.1 percent the year before).",
            "Late sailings rose to 7.5 percent (against 6.1 percent the year before).",
            0,
        ),
    ] {
        assert_eq!(without_citations(s), (want.to_string(), n), "{s}");
    }
}

/// Failing input (platform row PC1, amended 2026-10-15): a figure inside
/// parentheses left out of the claim's specifics. Parentheses that are no
/// citation and no locator hold specifics like any other words.
#[test]
fn a_figure_inside_other_parentheses_is_a_specific() {
    assert_eq!(
        texts("Prices rose 3.2% in 2023 (2.6 versus 1.8 percent)."),
        vec![
            (SpecificKind::Figure, "3.2%".into()),
            (SpecificKind::Date, "2023".into()),
            (SpecificKind::Figure, "2.6".into()),
            (SpecificKind::Figure, "1.8".into()),
        ]
    );
    let s = "Late sailings rose to 7.5 percent of the total in 2020 (against 6.1 percent the year before).";
    let got = sentence_specifics(s);
    let six = got.iter().find(|x| x.text == "6.1").expect("6.1 is named");
    assert_eq!(
        (six.kind, six.unit.as_deref()),
        (SpecificKind::Figure, Some("percent"))
    );
    // A date in parentheses is no citation: no name, or only a month, and a
    // month beside a number is no name either.
    assert_eq!(
        texts("Exports fell (in 2019) and (March 2019)."),
        vec![
            (SpecificKind::Date, "2019".into()),
            (SpecificKind::Date, "2019".into()),
        ]
    );
}

fn differ(claim: &str, passage: &str) -> Vec<(String, String)> {
    differing_specifics(
        &sentence_specifics(claim),
        &sentence_specifics(passage),
        passage,
    )
    .into_iter()
    .map(|(c, p)| (c.text, p.text))
    .collect()
}

#[test]
fn a_figure_stated_otherwise_is_paired_with_the_passages_of_its_kind() {
    let ledger = "The port recorded 412 sailings in 2019, and 31 of them left late.";
    assert_eq!(
        differ("The port recorded 512 sailings in 2019.", ledger),
        vec![("512".to_string(), "412".to_string())]
    );
    assert!(differ(
        "The port recorded 412 sailings in 2019 (Okafor 2019).",
        ledger
    )
    .is_empty());
    assert_eq!(
        differ(
            "Late sailings rose to 9% of the total in 2020.",
            "Late sailings rose to 7.5 percent of the total in 2020."
        ),
        vec![("9%".to_string(), "7.5".to_string())]
    );
    // The same figure in another form is no difference.
    assert!(differ(
        "Late sailings rose to 7.5% of the total in 2020.",
        "Late sailings rose to 7.5 percent of the total in 2020."
    )
    .is_empty());
}

#[test]
fn another_periods_figure_and_an_unanchored_date_are_no_difference() {
    let ledger = "The port recorded 412 sailings in 2019, and 31 of them left late.";
    assert!(differ("The port recorded 512 sailings in 2018.", ledger).is_empty());
    assert!(differ(
        "The port recorded 512 sailings in 2019.",
        "Late sailings rose to 7.5 percent of the total in 2020."
    )
    .is_empty());
    assert_eq!(
        differ(
            "In 1858 John Stuart Mill published the essay.",
            "In 1859 John Stuart Mill published the essay."
        ),
        vec![("1858".to_string(), "1859".to_string())]
    );
    // A unit apart is never a pair: a count of sailings is not a percentage.
    assert!(differ(
        "Late sailings were 40 percent of the total.",
        "The port recorded 412 sailings."
    )
    .is_empty());
}

/// The specific of `sentence` whose range reads `exact`.
fn found(sentence: &str, exact: &str) -> Option<Specific> {
    sentence_specifics(sentence)
        .into_iter()
        .find(|x| at(sentence, &x.range) == exact)
}

/// Failing input (live 2026-10-15, the daemon at :9741, Wealth of Nations):
/// "…upwards of forty-eight thousand pins a day." was `abstained` at
/// `class/no-specifics`, its figure written in words read as no specific. A
/// cardinal in words is a figure or a quantity like its digits, its range the
/// words as written ("a hundred" with its article), its text that range.
#[test]
fn a_cardinal_written_in_words_is_a_figure_or_a_quantity_at_its_words() {
    use SpecificKind::{Figure, Quantity};
    for (sentence, exact, kind, unit) in [
        (
            "Ten workers in a small pin factory could make upwards of forty-eight thousand pins a day.",
            "forty-eight thousand",
            Quantity,
            Some("pin"),
        ),
        (
            "Ten workers in a small pin factory could make upwards of forty-eight thousand pins a day.",
            "Ten",
            Quantity,
            Some("worker"),
        ),
        (
            "It was more than a hundred years after the first settlement of the colony.",
            "a hundred",
            Quantity,
            Some("year"),
        ),
        (
            "The town held eight hundred and fifty thousand people.",
            "eight hundred and fifty thousand",
            Quantity,
            Some("people"),
        ),
        ("The debt came to one million.", "one million", Figure, None),
        ("Late sailings rose by twenty percent.", "twenty", Figure, Some("percent")),
        ("Those ten persons, therefore, could make it.", "ten", Quantity, Some("person")),
        ("The ships numbered twelve, and no more.", "twelve", Figure, None),
    ] {
        let x = found(sentence, exact)
            .unwrap_or_else(|| panic!("no specific at `{exact}` in `{sentence}`: {:?}", sentence_specifics(sentence)));
        assert_eq!((x.kind, x.unit.as_deref()), (kind, unit), "{sentence}: {x:?}");
        assert_eq!(x.text, exact, "a number's text is its range");
    }
}

/// The words that look like numbers and assert none: a pronoun or a
/// determiner "one", an article, an ordinal, a fraction, a bare number word
/// under ten, a number word inside a name.
#[test]
fn a_pronoun_an_article_an_ordinal_and_a_bare_small_number_word_are_no_specifics() {
    for s in [
        "One of the workmen draws out the wire.",
        "Then one of the workmen draws out the wire.",
        "A man could hardly make one in a day.",
        "One must admit that the division of labour helps.",
        "No one could make it alone.",
        "It was after the first settlement of the colony.",
        "They paid a tenth part of the produce.",
        "The twenty-first year passed quietly.",
        "It was made in two, and then sold.",
        "They fought the Seven Years War.",
    ] {
        let numbers: Vec<_> = sentence_specifics(s)
            .into_iter()
            .filter(|x| x.kind != SpecificKind::Name)
            .collect();
        assert!(numbers.is_empty(), "{s}: {numbers:?}");
    } // A scale word after digits is their magnitude, not a number of its own.
    assert_eq!(
        texts("Wiz sold for $32 billion in cash."),
        vec![(SpecificKind::Figure, "$32".into())]
    );
}

/// "two or three distinct operations": by §2.15 each is a number with a
/// counted noun, so both ends of the alternation are quantities counting what
/// the last counts. A claim restating either end is no difference.
#[test]
fn both_ends_of_a_number_alternation_count_what_the_last_counts() {
    let s = "Making a pin is divided into two or three distinct operations.";
    let got: Vec<_> = sentence_specifics(s)
        .into_iter()
        .filter(|x| x.kind != SpecificKind::Name)
        .map(|x| (x.kind, x.text, x.unit))
        .collect();
    let distinct = Some("distinct".to_string());
    assert_eq!(
        got,
        vec![
            (SpecificKind::Quantity, "two".to_string(), distinct.clone()),
            (SpecificKind::Quantity, "three".to_string(), distinct),
        ]
    );
    assert!(differ("Making a pin takes 2 distinct operations.", s).is_empty());
}

/// Failing inputs (live 2026-10-15, :9741): a claim's digits against a
/// source's words were `unsupported` with `differing: []`, never
/// `contradicted`. Figures pair by value across the two forms, and the same
/// value in either form is no difference.
#[test]
fn a_figure_in_words_pairs_by_value_with_one_in_digits() {
    let pins = "Those ten persons, therefore, could make among them upwards of forty-eight thousand pins in a day.";
    assert_eq!(
        differ(
            "Ten workers in a small pin factory made 9,000 pins a day between them.",
            pins
        ),
        vec![("9,000".to_string(), "forty-eight thousand".to_string())]
    );
    assert_eq!(
        differ(
            "Silver, gold or diamond mines were found in Brazil fewer than 50 years after its first settlement.",
            "It was more than a hundred years after the first settlement of the Brazils, before any silver, gold, or diamond mines were discovered there."
        ),
        vec![("50".to_string(), "a hundred".to_string())]
    );
    assert!(differ("Ten workers made 48,000 pins a day.", pins).is_empty());
    assert!(differ("It took 100 years.", "It took a hundred years.").is_empty());
    let town = "The town held eight hundred and fifty thousand people.";
    assert!(differ("The town held 850,000 people.", town).is_empty());
    assert_eq!(
        differ("The town held 850 people.", town),
        vec![(
            "850".to_string(),
            "eight hundred and fifty thousand".to_string()
        )]
    );
    assert!(differ(
        "The town held one million people.",
        "The town held 1,000,000 people."
    )
    .is_empty());
}

fn bound_of(sentence: &str, exact: &str) -> Option<FigureBound> {
    found(sentence, exact).and_then(|s| s.bound)
}

#[test]
fn the_words_before_a_number_bound_it_the_longest_phrase_first() {
    let s = "It was more than a hundred years, at least 40 ships and up to 6 mines.";
    assert_eq!(bound_of(s, "a hundred"), Some(FigureBound::Above));
    assert_eq!(bound_of(s, "40"), Some(FigureBound::AtLeast));
    assert_eq!(bound_of(s, "6"), Some(FigureBound::AtMost));
    let s = "No more than 12 ships sailed, fewer than 30 men, upwards of 9,000 pins.";
    assert_eq!(bound_of(s, "12"), Some(FigureBound::AtMost));
    assert_eq!(bound_of(s, "30"), Some(FigureBound::Below));
    assert_eq!(bound_of(s, "9,000"), Some(FigureBound::Above));
    // A point, an approximation and a date take none.
    let s = "Over the next 25 years about 300 ships sailed, over 1750 or not.";
    assert_eq!(bound_of(s, "25"), None);
    assert_eq!(bound_of(s, "300"), None);
    assert_eq!(bound_of(s, "1750"), None);
}

/// The weaker-bound reading (operator, 2026-10-15): a bound the passage's
/// figure meets is no difference, one it cannot meet is. Live on
/// fa56bb72d, "more than 50 years" against Smith's "more than a hundred
/// years" was `contradicted`, differing (50, a hundred).
#[test]
fn a_bound_differs_only_from_a_figure_no_value_of_it_meets() {
    let smith = "It was more than a hundred years after the first settlement of \
                 the Brazils, before any silver mines were discovered there.";
    assert!(differ(
        "Silver mines were found more than 50 years after the settlement.",
        smith
    )
    .is_empty());
    assert_eq!(
        differ(
            "Silver mines were found fewer than 50 years after the settlement.",
            smith
        ),
        vec![("50".to_string(), "a hundred".to_string())]
    );
    let pins = "Those ten persons could make among them upwards of forty-eight \
                thousand pins in a day.";
    assert!(differ("Ten persons made upwards of 9,000 pins a day.", pins).is_empty());
    assert_eq!(
        differ("Ten persons made 4,800 pins a day.", pins),
        vec![("4,800".to_string(), "forty-eight thousand".to_string())]
    );
    // At the edge: "more than 100" leaves out 100, "at least 100" keeps it.
    let ledger = "The port recorded 100 sailings in 2019.";
    assert!(differ("The port recorded at least 100 sailings in 2019.", ledger).is_empty());
    assert!(differ("The port recorded up to 120 sailings in 2019.", ledger).is_empty());
    assert_eq!(
        differ("The port recorded under 90 sailings in 2019.", ledger),
        vec![("90".to_string(), "100".to_string())]
    );
}
