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
    };
    let inventory = Inventory {
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
        rows: vec![row(
            "both",
            vec![
                Check::TokenCount,
                Check::Scenario {
                    what: "parity".into(),
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
