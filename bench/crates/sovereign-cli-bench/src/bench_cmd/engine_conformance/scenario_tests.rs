// SPDX-License-Identifier: AGPL-3.0-or-later
//! `judge --scenario`: a row's scenario check takes the verdict the scenario
//! file gives it, each verdict watched passing and failing through the judge.

use super::check::{CellVerdict, Check, FailCause};
use super::inventory::{Inventory, Prediction};
use super::record::CaseRecord;
use super::tests::{case, is_differs, row};
use super::verdict;

#[test]
fn a_scenario_verdict_decides_the_rows_scenario_check() {
    let scenario = || Check::Scenario {
        what: "parity".into(),
        driver: None,
    };
    let inventory = Inventory {
        retired: Vec::new(),
        rows: vec![
            row(
                "both",
                vec![Check::TokenCount, scenario()],
                Prediction::Pass,
            ),
            row("lone", vec![scenario()], Prediction::Pass),
            row("unnamed", vec![scenario()], Prediction::Pass),
        ],
    };
    let records: Vec<CaseRecord> = ["both", "lone", "unnamed"]
        .iter()
        .flat_map(|r| {
            [
                case(r, "embedded", r, Some(5)),
                case(r, "llama-server", r, Some(5)),
            ]
        })
        .collect();
    let judged = |scenarios: &verdict::Scenarios, id: &str| {
        let v = verdict::judge(&inventory, &records, scenarios)
            .into_iter()
            .find(|v| v.row == id)
            .unwrap();
        (v.verdict, v.as_predicted, v.detail)
    };
    let all = |cell: CellVerdict| -> verdict::Scenarios {
        [
            ("both".to_string(), cell.clone()),
            ("lone".to_string(), cell),
        ]
        .into_iter()
        .collect()
    };

    let passed = all(CellVerdict::Passed);
    assert_eq!(judged(&passed, "both"), ("passed", Some(true), None));
    assert_eq!(judged(&passed, "lone"), ("passed", Some(true), None));

    let failed = all(CellVerdict::Failed {
        cause: FailCause::Differs,
        detail: "scenario: 1 of 9 inputs differ".into(),
    });
    for id in ["both", "lone"] {
        let (verdict, as_predicted, detail) = judged(&failed, id);
        assert_eq!((verdict, as_predicted), ("failed", Some(false)), "{id}");
        assert!(detail.unwrap().contains("1 of 9 inputs differ"), "{id}");
    }

    // A row the file does not name keeps the scenario's could-not-judge.
    let (verdict, _, detail) = judged(&passed, "unnamed");
    assert_eq!(verdict, "could-not-judge");
    assert!(detail.unwrap().contains("parity"));
}

#[test]
fn a_passing_scenario_does_not_hide_a_failing_check_beside_it() {
    let inventory = Inventory {
        retired: Vec::new(),
        rows: vec![row(
            "both",
            vec![
                Check::TokenCount,
                Check::Scenario {
                    what: "parity".into(),
                    driver: None,
                },
            ],
            Prediction::Pass,
        )],
    };
    let records = vec![
        case("a", "embedded", "both", Some(5)),
        case("a", "llama-server", "both", Some(6)),
    ];
    let scenarios: verdict::Scenarios = [("both".to_string(), CellVerdict::Passed)]
        .into_iter()
        .collect();
    let v = &verdict::judge(&inventory, &records, &scenarios)[0];
    assert_eq!((v.verdict, v.cause), ("failed", Some("differs")));
}

#[test]
fn a_scenario_file_is_read_per_row_and_refuses_what_it_cannot_mean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scenario.jsonl");
    std::fs::write(
        &path,
        "{\"row\":\"a\",\"verdict\":\"passed\",\"detail\":\"9 of 9 agree\"}\n\
         {\"row\":\"b\",\"verdict\":\"failed\",\"detail\":\"1 of 9 differ\"}\n\
         {\"row\":\"c\",\"verdict\":\"could-not-judge\",\"detail\":\"no compile\"}\n",
    )
    .unwrap();
    let s = super::read_scenarios(&path).unwrap();
    assert_eq!(s["a"], CellVerdict::Passed);
    assert!(is_differs(&s["b"]));
    assert_eq!(
        s["c"],
        CellVerdict::CouldNotJudge("scenario: no compile".into())
    );

    std::fs::write(&path, "{\"row\":\"a\",\"verdict\":\"pass\"}\n").unwrap();
    assert!(super::read_scenarios(&path)
        .unwrap_err()
        .contains("is not passed"));
    std::fs::write(
        &path,
        "{\"row\":\"a\",\"verdict\":\"passed\"}\n{\"row\":\"a\",\"verdict\":\"failed\"}\n",
    )
    .unwrap();
    assert!(super::read_scenarios(&path)
        .unwrap_err()
        .contains("appears twice"));
}

// ── the three scenarios the driveR runs itself (L I3) and host.lifecycle
//    (L I4), each shown passing and failing ─────────────────────────────────

use serde_json::json;

use super::record::{Output, SplitLine, StreamUsageFacet};
use super::scenario;

fn target(facets: super::record::Facets) -> CaseRecord {
    CaseRecord {
        case_id: "c".into(),
        target: "llama-server".into(),
        rows: vec!["r".into()],
        request: json!({}),
        outcome: super::record::Outcome::Ok,
        facets,
    }
}

#[test]
fn a_split_line_passes_only_when_the_reader_delivered_the_whole_answer() {
    let mut f = super::record::Facets::default();
    f.output = Some(Output {
        text: "the whole answer".into(),
        ..Default::default()
    });
    f.split_line = Some(SplitLine {
        text: "the whole answer".into(),
        deltas: 3,
    });
    assert_eq!(
        scenario::split_line(&target(f.clone())),
        CellVerdict::Passed
    );

    // The line was dropped: the chunked read lost text.
    f.split_line = Some(SplitLine {
        text: "the whole".into(),
        deltas: 2,
    });
    assert!(is_differs(&scenario::split_line(&target(f.clone()))));

    // No delta at all where the whole stream had text is also a loss.
    f.split_line = Some(SplitLine {
        text: String::new(),
        deltas: 0,
    });
    assert!(is_differs(&scenario::split_line(&target(f.clone()))));

    // Unobserved is could-not-judge, never a pass.
    f.split_line = None;
    assert!(matches!(
        scenario::split_line(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn admission_needs_the_admitted_call_served_and_the_fresh_one_shed() {
    let mut f = super::record::Facets::default();
    f.admission = vec!["shed:unadmitted".into(), "served:admitted".into()];
    assert_eq!(scenario::admission(&target(f.clone())), CellVerdict::Passed);

    // Both served (the router, which sheds nothing): a difference, not a pass.
    f.admission = vec!["served:unadmitted".into(), "served:admitted".into()];
    assert!(is_differs(&scenario::admission(&target(f.clone()))));

    // The admitted call was the one shed: the policy inverted.
    f.admission = vec!["shed:admitted".into(), "served:unadmitted".into()];
    assert!(is_differs(&scenario::admission(&target(f.clone()))));

    f.admission = vec![];
    assert!(matches!(
        scenario::admission(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn lifecycle_passes_when_an_ok_names_a_resident_model_and_a_refusal_is_honest() {
    let mut f = super::record::Facets::default();
    f.host_observed = [("resident_models".to_string(), json!(["m"]))]
        .into_iter()
        .collect();
    f.lifecycle = [
        (
            "warmup_loaded".to_string(),
            json!({"kind": "ok", "model": "m"}),
        ),
        (
            "extra_loaded".to_string(),
            json!({"kind": "err", "message": "no extra slots"}),
        ),
        (
            "extra_unloaded".to_string(),
            json!({"kind": "err", "message": "no extra slots"}),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(scenario::lifecycle(&target(f.clone())), CellVerdict::Passed);

    // An Ok over a model the router does not list: a silent failure.
    f.lifecycle.insert(
        "warmup_loaded".into(),
        json!({"kind": "ok", "model": "not-there"}),
    );
    assert!(is_differs(&scenario::lifecycle(&target(f.clone()))));

    // Not asked where the row needed it: a gap in the instrument — even with
    // a sound warmup beside it.
    f.lifecycle
        .insert("warmup_loaded".into(), json!({"kind": "ok", "model": "m"}));
    f.lifecycle
        .insert("extra_loaded".into(), json!({"kind": "not-asked"}));
    assert!(matches!(
        scenario::lifecycle(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn a_corrupted_stream_passes_only_when_it_ends_in_an_error_frame() {
    let mut f = super::record::Facets::default();
    f.output = Some(Output {
        finish: Some("error".into()),
        frames: vec!["error".into()],
        ..Default::default()
    });
    assert_eq!(
        scenario::stream_error(&target(f.clone())),
        CellVerdict::Passed
    );

    // Ending in a clean stop is exactly the silent loss the row names.
    f.output = Some(Output {
        finish: Some("stop".into()),
        frames: vec!["token".into(), "finish".into()],
        ..Default::default()
    });
    assert!(is_differs(&scenario::stream_error(&target(f.clone()))));

    f.output = None;
    assert!(matches!(
        scenario::stream_error(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn admission_shape_passes_either_targets_own_answer_but_needs_the_admitted_call_served() {
    // The engine's shape: the admitted call served, the fresh one shed.
    let mut f = super::record::Facets::default();
    f.admission = vec!["shed:unadmitted".into(), "served:admitted".into()];
    assert_eq!(
        scenario::admission_shape(&target(f.clone())),
        CellVerdict::Passed
    );

    // The router's shape: no admission concept, both served, neither shed.
    // A verdict, not a failure, and it is what `fail:differs` predicts.
    f.admission = vec!["served:unadmitted".into(), "served:admitted".into()];
    assert_eq!(
        scenario::admission_shape(&target(f.clone())),
        CellVerdict::Passed
    );

    // The admitted call shed is the bound failing, whichever engine.
    f.admission = vec!["shed:admitted".into(), "served:unadmitted".into()];
    assert!(is_differs(&scenario::admission_shape(&target(f.clone()))));

    f.admission = Vec::new();
    assert!(matches!(
        scenario::admission_shape(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn stream_usage_counts_the_answers_tokens_either_way_they_are_counted() {
    let mut f = super::record::Facets::default();
    // The engine's own count: the answer's tokens.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 12,
        answer_tokens: Some(12),
        deltas: 3,
    });
    assert_eq!(
        scenario::stream_usage(&target(f.clone())),
        CellVerdict::Passed
    );

    // llama-server's count is what it evaluated: a draft head makes it the
    // number of deltas, not the answer's tokens. Still the answer's length.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 3,
        answer_tokens: Some(12),
        deltas: 3,
    });
    assert_eq!(
        scenario::stream_usage(&target(f.clone())),
        CellVerdict::Passed
    );

    // The prompt's count is a different number and fails.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 277,
        answer_tokens: Some(12),
        deltas: 3,
    });
    assert!(is_differs(&scenario::stream_usage(&target(f.clone()))));

    // Two tokenizers counting one templated answer: 55 against the 33 the
    // other names is still the answer's count (the engine's template tokens
    // cost more than the server's tokenizer makes of the same turn).
    f.stream_usage = Some(StreamUsageFacet {
        completion: 55,
        answer_tokens: Some(33),
        deltas: 1,
    });
    assert_eq!(
        scenario::stream_usage(&target(f.clone())),
        CellVerdict::Passed
    );

    // A count far past the answer's own is another turn's.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 120,
        answer_tokens: Some(33),
        deltas: 1,
    });
    assert!(is_differs(&scenario::stream_usage(&target(f.clone()))));

    // A frame that carried only the count, with no tie recorded, is
    // could-not-judge: the instrument saw nothing to hold it to.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 12,
        answer_tokens: None,
        deltas: 0,
    });
    assert!(matches!(
        scenario::stream_usage(&target(f.clone())),
        CellVerdict::CouldNotJudge(_)
    ));

    // With the answer's length, the frame is judged on it.
    f.stream_usage = Some(StreamUsageFacet {
        completion: 12,
        answer_tokens: Some(40),
        deltas: 0,
    });
    assert!(is_differs(&scenario::stream_usage(&target(f.clone()))));

    f.stream_usage = None;
    assert!(matches!(
        scenario::stream_usage(&target(f)),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn a_named_driver_grades_the_record_and_an_unknown_one_is_reported() {
    let mut f = super::record::Facets::default();
    f.admission = vec!["shed:unadmitted".into(), "served:admitted".into()];
    assert_eq!(
        scenario::grade("admission", &target(f.clone())),
        Some(CellVerdict::Passed)
    );
    assert_eq!(scenario::grade("no-such-driver", &target(f)), None);
    // A scenario naming an unknown driver is could-not-judge through the
    // check, not a pass.
    let check = Check::Scenario {
        what: "x".into(),
        driver: Some("no-such-driver".into()),
    };
    assert!(matches!(
        check.judge(None, &target(Default::default())),
        CellVerdict::CouldNotJudge(_)
    ));
}
