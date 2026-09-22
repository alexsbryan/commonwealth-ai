use super::*;

fn d(facet: DemandFacet, text: &str, covered: CoverageLevel) -> Demand {
    Demand {
        facet,
        text: text.to_string(),
        covered,
    }
}

/// The ANS shape: the hoard is in the pool, a second named entity is not.
fn kyparissia() -> Vec<Demand> {
    vec![
        d(
            DemandFacet::Query,
            "Which mints are represented in the Kyparissia hoard?",
            CoverageLevel::Retrieved,
        ),
        d(
            DemandFacet::Entity,
            "Kyparissia hoard",
            CoverageLevel::Retrieved,
        ),
        d(DemandFacet::Entity, "Troxell 1997", CoverageLevel::Absent),
        d(
            DemandFacet::SubQuestion,
            "mints represented hoard",
            CoverageLevel::Absent,
        ),
    ]
}

#[test]
fn brief_states_found_and_missing_named_facets() {
    let brief = render_coverage_brief(&kyparissia());
    assert!(brief.contains("Named in these passages: Kyparissia hoard"));
    assert!(brief.contains("Not found by name in these passages: Troxell 1997"));
}

#[test]
fn brief_never_renders_the_query_or_heuristic_subquestions() {
    let brief = render_coverage_brief(&kyparissia());
    assert!(
        !brief.contains("Which mints"),
        "the Query facet is Retrieved on any non-empty pool — it establishes nothing"
    );
    assert!(
        !brief.contains("mints represented hoard"),
        "a heuristic sub-question's absence is weak evidence and must not prime a decline"
    );
}

#[test]
fn brief_is_empty_when_nothing_named_was_checked() {
    let only_query = vec![d(
        DemandFacet::Query,
        "What is epistemology?",
        CoverageLevel::Retrieved,
    )];
    assert_eq!(render_coverage_brief(&only_query), "");
}

#[test]
fn brief_never_says_not_in_your_sources() {
    // "not in your sources" is in `answer_declines`' list and overstates a
    // lexical miss as a fact about the whole corpus.
    let brief = render_coverage_brief(&kyparissia()).to_lowercase();
    assert!(!brief.contains("not in your sources"));
}

#[test]
fn uncovered_ask_names_only_absent_entities() {
    let ask = uncovered_ask(&kyparissia()).expect("one entity is absent");
    assert_eq!(
        ask,
        "Not found by name in the retrieved passages: Troxell 1997"
    );
    let all_found = vec![d(
        DemandFacet::Entity,
        "Kyparissia hoard",
        CoverageLevel::Retrieved,
    )];
    assert_eq!(uncovered_ask(&all_found), None);
}

#[test]
fn abstention_outranks_coverage_and_bool_maps_as_before() {
    // Independent of the flag: an abstained turn is always `Abstained`.
    assert_eq!(
        GapTrigger::for_turn(true, &kyparissia()),
        GapTrigger::Abstained
    );
    assert_eq!(GapTrigger::from(true), GapTrigger::Abstained);
    assert_eq!(GapTrigger::from(false), GapTrigger::Answered);
}

#[test]
fn metadata_without_a_ledger_falls_back_to_the_abstention_signal() {
    let meta = serde_json::json!({ "grounding_gate": { "action": "released" } });
    assert_eq!(
        GapTrigger::for_metadata(false, Some(&meta)),
        GapTrigger::Answered
    );
    assert_eq!(GapTrigger::for_metadata(true, None), GapTrigger::Abstained);
}
