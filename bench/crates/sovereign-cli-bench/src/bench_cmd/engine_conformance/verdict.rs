// SPDX-License-Identifier: AGPL-3.0-or-later
//! From cell verdicts to one verdict per row and target pair, set beside the
//! verdict the inventory predicted.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::check::{CellVerdict, Check, FailCause};
use super::inventory::{Inventory, Prediction, Row};
use super::record::CaseRecord;

/// The reference target when a row names no pairs.
pub const REFERENCE: &str = "embedded";

/// A row's verdict for one target pair, over every case the row has.
#[derive(Debug, Clone, Serialize)]
pub struct RowVerdict {
    /// The row id.
    pub row: String,
    /// The reference target (`-` for a target-only row).
    pub reference: String,
    /// The target judged.
    pub target: String,
    /// `passed`, `failed`, `could-not-judge` or `never-ran`.
    pub verdict: &'static str,
    /// `refused` or `differs`, for a failed verdict.
    pub cause: Option<&'static str>,
    /// The first failure or the first reason a case could not be judged.
    pub detail: Option<String>,
    /// Cases judged, and how many of them failed.
    pub cases: usize,
    /// Cases that failed.
    pub failed: usize,
    /// The verdict predicted before the first run.
    pub predicted: &'static str,
    /// Whether the verdict is the one predicted. `None` when nothing was
    /// judged.
    pub as_predicted: Option<bool>,
}

/// Scenario verdicts by row id (`judge --scenario`): what a row's
/// [`Check::Scenario`] comes to on every case of that row.
pub type Scenarios = BTreeMap<String, CellVerdict>;

/// Judge every row against the records. A target run twice (A/A) is judged
/// on its first run, and only where its two runs agree under the row's own
/// pairwise checks: a cell that differs between two runs of one engine says
/// nothing about the difference between two engines.
pub fn judge(
    inventory: &Inventory,
    records: &[CaseRecord],
    scenarios: &Scenarios,
) -> Vec<RowVerdict> {
    let mut by_case: BTreeMap<(&str, &str), &CaseRecord> = BTreeMap::new();
    let mut repeat: BTreeMap<(&str, &str), &CaseRecord> = BTreeMap::new();
    for r in records {
        let key = (r.case_id.as_str(), r.target.as_str());
        if by_case.contains_key(&key) {
            repeat.entry(key).or_insert(r);
        } else {
            by_case.insert(key, r);
        }
    }
    let targets: BTreeSet<&str> = records.iter().map(|r| r.target.as_str()).collect();
    let mut out = Vec::new();
    for row in &inventory.rows {
        for (reference, target) in pairs(row, &targets) {
            let cases: BTreeSet<&str> = records
                .iter()
                .filter(|r| r.target == target && r.rows.iter().any(|id| *id == row.id))
                .map(|r| r.case_id.as_str())
                .collect();
            let cells: Vec<(&str, CellVerdict)> = cases
                .iter()
                .map(|case| {
                    let t = by_case[&(*case, target.as_str())];
                    let r = by_case.get(&(*case, reference.as_str())).copied();
                    let unstable =
                        [target.as_str(), reference.as_str()]
                            .into_iter()
                            .find_map(|who| {
                                let key = (*case, who);
                                a_a_difference(row, by_case.get(&key)?, repeat.get(&key)?)
                            });
                    let cell = match unstable {
                        Some(why) => CellVerdict::CouldNotJudge(why),
                        None => judge_case(row, r, t, scenarios.get(&row.id)),
                    };
                    (*case, cell)
                })
                .collect();
            out.push(roll_up(row, &reference, &target, &cells));
        }
    }
    out
}

fn pairs(row: &Row, targets: &BTreeSet<&str>) -> Vec<(String, String)> {
    if !row.pairs.is_empty() {
        return row.pairs.clone();
    }
    targets
        .iter()
        .filter(|t| **t != REFERENCE)
        .map(|t| (REFERENCE.to_string(), t.to_string()))
        .collect()
}

/// How two runs of one target differ under the row's pairwise checks, if
/// they do. A target-only check judges each run on its own, so a failure
/// there is a failure, not instability, and is left to the first run.
fn a_a_difference(row: &Row, first: &CaseRecord, second: &CaseRecord) -> Option<String> {
    let why = row
        .judge
        .iter()
        .filter(|c| !c.target_only())
        .find_map(|c| match c.judge(Some(first), second) {
            CellVerdict::Failed { detail, .. } => Some(detail),
            _ => None,
        })?;
    tracing::debug!(target: "engine_conformance", row = %row.id, case = %first.case_id, who = %first.target, %why, "A/A difference");
    Some(format!("A/A: two runs of {} differ: {why}", first.target))
}

/// Every check on one case. A scenario check takes the row's scenario
/// verdict when one was given, and stays could-not-judge otherwise.
fn judge_case(
    row: &Row,
    reference: Option<&CaseRecord>,
    target: &CaseRecord,
    scenario: Option<&CellVerdict>,
) -> CellVerdict {
    let mut unjudged = None;
    for check in &row.judge {
        let cell = match (check, scenario) {
            (Check::Scenario { .. }, Some(v)) => v.clone(),
            _ => check.judge(reference, target),
        };
        match cell {
            CellVerdict::Passed => {}
            failed @ CellVerdict::Failed { .. } => return failed,
            CellVerdict::CouldNotJudge(why) => {
                unjudged.get_or_insert(why);
            }
        }
    }
    unjudged.map_or(CellVerdict::Passed, CellVerdict::CouldNotJudge)
}

fn roll_up(row: &Row, reference: &str, target: &str, cells: &[(&str, CellVerdict)]) -> RowVerdict {
    let failures: Vec<_> = cells
        .iter()
        .filter_map(|(case, v)| match v {
            CellVerdict::Failed { cause, detail } => Some((*case, *cause, detail)),
            _ => None,
        })
        .collect();
    let unjudged = cells.iter().find_map(|(case, v)| match v {
        CellVerdict::CouldNotJudge(why) => Some(format!("{case}: {why}")),
        _ => None,
    });
    let (verdict, cause, detail) = if let Some((case, cause, detail)) = failures.first() {
        let cause = match cause {
            FailCause::Refused => "refused",
            FailCause::Differs => "differs",
        };
        ("failed", Some(cause), Some(format!("{case}: {detail}")))
    } else if cells.is_empty() {
        ("never-ran", None, None)
    } else if unjudged.is_some() {
        ("could-not-judge", None, unjudged)
    } else {
        ("passed", None, None)
    };
    let as_predicted = match (verdict, cause) {
        ("passed", _) => Some(row.predict == Prediction::Pass),
        ("failed", Some("refused")) => Some(row.predict == Prediction::Refused),
        ("failed", _) => Some(matches!(
            row.predict,
            Prediction::Silent | Prediction::Differs
        )),
        _ => None,
    };
    RowVerdict {
        row: row.id.clone(),
        reference: if row.judge.iter().all(|c| c.target_only()) {
            "-".into()
        } else {
            reference.into()
        },
        target: target.into(),
        verdict,
        cause,
        detail,
        cases: cells.len(),
        failed: failures.len(),
        predicted: match row.predict {
            Prediction::Pass => "pass",
            Prediction::Silent => "fail:silent",
            Prediction::Differs => "fail:differs",
            Prediction::Refused => "fail:refused",
        },
        as_predicted,
    }
}
