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
