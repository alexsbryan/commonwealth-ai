use super::*;

fn chunk(title: &str, content: &str, corpus: &str) -> corpus_engine::ScoredChunk {
    corpus_engine::ScoredChunk {
        content: content.to_string(),
        title: Some(title.to_string()),
        url: None,
        corpus_id: corpus.to_string(),
        score: 1.0,
        metadata: std::collections::HashMap::new(),
        chunk_id: None,
        source_doc_id: None,
        vector_distance: None,
        // Fixture chunk: nothing acquired it (TOPOLOGY §10 rung 9.1).
        provenance: corpus_engine::index::ChunkProvenance::manufactured("test_fixture"),
    }
}

#[test]
fn demands_build_and_stamp() {
    let entities = vec!["Isaac Newton".to_string(), "Einstein".to_string()];
    let mut demands = build_demands(
        "How did Newton and Einstein differ on gravity?",
        &Intent::KnowledgeQuery,
        &entities,
        None,
    );
    assert_eq!(demands[0].facet, DemandFacet::Query);
    assert!(demands
        .iter()
        .any(|d| d.facet == DemandFacet::Entity && d.text == "Isaac Newton"));
    let chunks = vec![chunk(
        "Isaac Newton",
        "newton's law of universal gravitation",
        "wikipedia",
    )];
    stamp_coverage(&mut demands, &chunks);
    assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // pool non-empty
    let newton = demands
        .iter()
        .find(|d| d.text == "Isaac Newton")
        .expect("newton demand");
    assert_eq!(newton.covered, CoverageLevel::Retrieved);
    let einstein = demands
        .iter()
        .find(|d| d.text == "Einstein")
        .expect("einstein demand");
    assert_eq!(einstein.covered, CoverageLevel::Absent);
}

#[test]
fn build_demands_folds_in_the_llm_plan() {
    use crate::runtime::retrieval_pipeline::{DemandPlan, StanceContrast};
    let plan = DemandPlan {
        sub_queries: vec!["general relativity gravity".into()],
        entities: vec![],
        stance_contrast: Some(StanceContrast {
            axis: "the nature of gravity".into(),
            poles: vec!["action at a distance".into(), "spacetime curvature".into()],
        }),
        section_terms: vec!["reception".into()],
    };
    let demands = build_demands(
        "How did Newton and Einstein differ on gravity?",
        &Intent::KnowledgeQuery,
        &[],
        Some(&plan),
    );
    // Stance poles → both sides demanded.
    assert!(demands
        .iter()
        .any(|d| d.facet == DemandFacet::Stance && d.text == "action at a distance"));
    assert!(demands
        .iter()
        .any(|d| d.facet == DemandFacet::Stance && d.text == "spacetime curvature"));
    // Section term.
    assert!(demands
        .iter()
        .any(|d| d.facet == DemandFacet::Section && d.text == "reception"));
    // Plan sub-query.
    assert!(demands
        .iter()
        .any(|d| d.facet == DemandFacet::SubQuestion && d.text == "general relativity gravity"));
}

#[test]
fn stance_and_section_facets_stamp_and_gap() {
    let mut demands = vec![
        Demand {
            facet: DemandFacet::Stance,
            text: "spacetime curvature".into(),
            covered: CoverageLevel::Absent,
        },
        Demand {
            facet: DemandFacet::Section,
            text: "reception".into(),
            covered: CoverageLevel::Absent,
        },
    ];
    // A chunk covering the stance pole (surface-form containment).
    let chunks = vec![chunk(
        "General relativity",
        "gravity as spacetime curvature, per Einstein",
        "wikipedia",
    )];
    stamp_coverage(&mut demands, &chunks);
    assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // stance pole present
    assert_eq!(demands[1].covered, CoverageLevel::Absent); // no "reception" text
                                                           // The uncovered Section facet emits a gap with its own statement.
    let gaps = finish_demands(&mut demands, None, false, Some(GapCoverage::TopicUncovered));
    assert!(gaps
        .iter()
        .any(|g| g.statement.contains("reception") && g.statement.contains("section")));
}

#[test]
fn finish_upgrades_supported_and_emits_gaps() {
    let mut demands = vec![
        Demand {
            facet: DemandFacet::Query,
            text: "q".into(),
            covered: CoverageLevel::Retrieved,
        },
        Demand {
            facet: DemandFacet::Entity,
            text: "Szilard".into(),
            covered: CoverageLevel::Absent,
        },
    ];
    let claims = vec![GateClaim {
        text: "supported claim".into(),
        supported: true,
        failed_once: false,
        unjudged: false,
        violation_prob: None,
        address: None,
    }];
    let gaps = finish_demands(
        &mut demands,
        Some(&claims),
        false,
        Some(GapCoverage::TopicUncovered),
    );
    assert_eq!(demands[0].covered, CoverageLevel::Supported);
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].demand_idx, 1);
    assert_eq!(gaps[0].coverage, GapCoverage::TopicUncovered);
}

#[test]
fn abstained_turn_gaps_the_query_as_claim_uncovered() {
    let mut demands = vec![Demand {
        facet: DemandFacet::Query,
        text: "who is Heat".into(),
        covered: CoverageLevel::Retrieved,
    }];
    let gaps = finish_demands(&mut demands, None, true, None);
    assert_eq!(demands[0].covered, CoverageLevel::Retrieved); // never upgraded on abstain
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].coverage, GapCoverage::ClaimUncovered);
}

/// The probe's calibrated verdict outranks the weak `Retrieved`
/// affinity stamp on abstained turns: top-k retrieval returns
/// SOMETHING for any query, so an OOD question over distractors
/// still stamps Retrieved — but the probe read the sealed corpus at
/// 0.17-0.49 and its TopicUncovered must reach the gap (observed
/// mis-route: ood-australia-capital, 2026-07-20).
#[test]
fn probe_verdict_outranks_retrieved_stamp_on_abstained_turns() {
    let mut demands = vec![Demand {
        facet: DemandFacet::Query,
        text: "what is the capital of Australia".into(),
        covered: CoverageLevel::Retrieved,
    }];
    let gaps = finish_demands(&mut demands, None, true, Some(GapCoverage::TopicUncovered));
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].coverage, GapCoverage::TopicUncovered);
    // And an in-topic probe verdict keeps the claim-level routing.
    let mut demands = vec![Demand {
        facet: DemandFacet::Query,
        text: "who is Verloc's wife".into(),
        covered: CoverageLevel::Retrieved,
    }];
    let gaps = finish_demands(&mut demands, None, true, Some(GapCoverage::ClaimUncovered));
    assert_eq!(gaps[0].coverage, GapCoverage::ClaimUncovered);
}

#[test]
fn fail_open_recall_is_visible() {
    let recalled = vec![RecalledMemoryProv {
        id: "mem-2".into(),
        content: "mentioned a trip".into(),
        created_at: 0,
        kind: None,
        source_memory_ids: vec![],
        confidence: Some(0.4),
    }];
    let rv = RecallVerificationProv {
        grounded: true,
        fail_open: true,
        referenced: Some(1),
    };
    let state = assemble_epistemic_state(EpistemicInputs {
        recalled: &recalled,
        recall_verification: Some(&rv),
        ..EpistemicInputs::over(PoolContext::none())
    });
    assert_eq!(state.holdings[0].verification, Verification::FailOpen);
    assert!(matches!(
        &state.holdings[0].provenance,
        Provenance::Memory {
            band: MemoryBand::Tentative,
            ..
        }
    ));
}
