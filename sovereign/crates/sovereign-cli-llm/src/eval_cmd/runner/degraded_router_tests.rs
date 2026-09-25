// SPDX-License-Identifier: AGPL-3.0-or-later
//! `degraded_router`'s tests, in their own file under `#[path]` from
//! `runner.rs`: keeping them inline put that file past its arch-gate slack
//! (ARCH §3.1) when the grounding gate's echo field landed (a313c9c18).
//! The module stays a child of `runner`, so `super::*` still reaches the
//! private items these tests read.

use super::*;
use sovereign_contracts::types::{ResponseProvenance, RouterStamp};

/// The legacy shape from `router_stamp_tests::the_field_is_backward_compatible`
/// — the minimum a `ResponseProvenance` needs to parse.
fn provenance() -> ResponseProvenance {
    serde_json::from_str(
        r#"{"intent":"SIMPLE","search_method":null,"sources":[],
            "inference_backend":"m","oicp_match":null,
            "total_latency_ms":1,"tokens_used":2}"#,
    )
    .expect("legacy provenance parses")
}

/// SERIALISED BY SERDE, never hand-written, and that is the whole point of
/// this test. The key this reads (`router`) and the four field names inside
/// it are `ResponseProvenance`'s and `RouterStamp`'s to choose. A hand-typed
/// `"router"` here would keep passing after a `#[serde(rename)]` renamed the
/// wire field, and the detector would then silently never fire again —
/// which is exactly the failure it exists to catch (ARCH §18.1).
fn as_metadata(stamp: Option<RouterStamp>) -> serde_json::Value {
    let mut p = provenance();
    p.router = stamp;
    serde_json::to_value(&p).expect("provenance serialises")
}

#[test]
fn a_turn_routed_by_no_classifier_is_not_a_measurement() {
    let degraded = as_metadata(Some(RouterStamp::from_liveness(false, false, false, false)));
    let why =
        degraded_router(Some(&degraded)).expect("all four classifiers dead is the degraded host");
    assert!(
        why.contains("not a measurement"),
        "the reason reaches the report and one example is printed by \
         `classify_retrieval` — it has to say what happened; got {why}"
    );
}

/// The two ways a healthy run reaches here, and neither may be excluded.
/// Collapsing either into "degraded" would silently shrink every bank.
#[test]
fn a_partial_router_and_an_absent_one_are_both_measurements() {
    let partial = as_metadata(Some(RouterStamp::from_liveness(true, false, false, false)));
    assert_eq!(
        degraded_router(Some(&partial)),
        None,
        "one live classifier still routed — degradation is a degree, and \
         `routed_by_none` is the one implementation of the question (§10.6)"
    );

    let absent = as_metadata(None);
    assert_eq!(
        degraded_router(Some(&absent)),
        None,
        "a turn that does not REPORT a router is not a turn that reports a \
         dead one; old messages have no `router` key at all"
    );

    assert_eq!(
        degraded_router(None),
        None,
        "no provenance block at all is not evidence of degradation"
    );
}

/// The exclusion has to survive the trip through `EvalResult`, because that
/// is the only shape `drop_unmeasured` can see.
#[test]
fn the_degraded_row_carries_the_error_drop_unmeasured_filters_on() {
    let row = EvalResult {
        error: None,
        question_id: "q1".into(),
        category: "c".into(),
        question: "why".into(),
        retrieved: Vec::new(),
        source_score: score_sources(&[], &[]).into(),
        fact_score: score_facts_in_text(&[], "").into(),
        embed_ms: 0,
        search_ms: 0,
        corpora_hit: Vec::new(),
        vector_eligible: false,
        synth: None,
        loose_source_score: None,
        loose_source_evidence: Vec::new(),
        essay_readiness: None,
        atlas_navigation: Vec::new(),
        meta_atlas_hits: Vec::new(),
        atlas_walk: None,
    };
    let degraded = as_metadata(Some(RouterStamp::default()));
    let why = degraded_router(Some(&degraded)).expect("default stamp is all-false");
    assert!(
        row.with_error(why).error.is_some(),
        "a row whose scores are arithmetic over an unrouted answer must not \
         reach the baseline diff as a measurement"
    );
}
