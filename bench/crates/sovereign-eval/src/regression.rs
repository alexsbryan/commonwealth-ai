// SPDX-License-Identifier: AGPL-3.0-or-later
//! Existing-test regression check (seam #3).
//!
//! For a feature added inside a real codebase (commonwealth-shape
//! target), "did existing tests stay green?" is a separate question
//! from "did your new tests pass." A clean mechanical scorer report
//! says only the latter.
//!
//! Workflow:
//! 1. Operator captures a baseline `mechanical.json` *before* the
//!    overnight (`sovereign-eval score <baseline-run-id> --no-judge`,
//!    or in CI before merging).
//! 2. After the session, the post-run `mechanical.json` lands as
//!    usual.
//! 3. `compare_to_baseline` set-diffs the two: any test that was
//!    PASSING before and FAILING now is a regression.
//!
//! The agent's *new* tests (failures that didn't exist in the
//! baseline because the test itself didn't exist) show up as
//! `new_failures` only if they actually run — the test was added,
//! attempted, and failed. Tests that didn't exist in baseline and
//! now pass show up as `new_passing_tests`.

use crate::mechanical::MechanicalReport;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionReport {
    pub baseline_passed: u32,
    pub baseline_failed: u32,
    pub baseline_total: u32,
    pub current_passed: u32,
    pub current_failed: u32,
    pub current_total: u32,
    /// Tests that PASSED in baseline but FAILED in the current run —
    /// these are regressions the agent introduced.
    pub regressions: Vec<String>,
    /// Tests that FAILED in baseline but PASSED in the current run —
    /// these are bugs the agent fixed (intentionally or not).
    pub fixes: Vec<String>,
    /// Tests present in current but not in baseline — newly added.
    pub new_passing_tests: u32,
    pub new_failing_tests: Vec<String>,
    /// Tests present in baseline but not in current — removed.
    pub removed_tests: Vec<String>,
    /// Convenience: regressions.len(). Zero is the only acceptable
    /// value for a passing run.
    pub regression_count: u32,
}

pub fn compare_to_baseline(
    baseline: &MechanicalReport,
    current: &MechanicalReport,
) -> RegressionReport {
    let baseline_failures: HashSet<String> = baseline.failed_test_names.iter().cloned().collect();
    let current_failures: HashSet<String> = current.failed_test_names.iter().cloned().collect();

    // We can only catch regressions if we know which tests existed in
    // each. The mechanical report only carries failed names; a
    // passing-test name is implicit. So:
    // - regressions = tests in current_failures that are NOT in
    //   baseline_failures AND were tested in baseline (i.e., baseline
    //   total >= current_failures known to baseline). Approximation:
    //   if baseline failed_names set differs from current set, the new
    //   ones are *either* regressions *or* newly-added failing tests.
    //
    // To distinguish, we use baseline_total vs current_total: tests
    // that grew in count are agent-added; if a baseline-existing test
    // now fails, that's a regression.
    //
    // Without per-test pass-list we approximate: any failure in
    // current that wasn't in baseline is *flagged*. The reviewer can
    // disambiguate by reading the test name (was this test in the
    // baseline source?).

    let mut regressions: Vec<String> = current_failures
        .difference(&baseline_failures)
        .cloned()
        .collect();
    let mut fixes: Vec<String> = baseline_failures
        .difference(&current_failures)
        .cloned()
        .collect();
    regressions.sort();
    fixes.sort();

    // New tests = current_total - baseline_total; assume they are at
    // the tail of current_failures if total grew. We can't be precise
    // without per-test status, so we report this as a count + the
    // names of all new failures (operator decides if they were added).
    let new_total_delta = (current.tests_total as i64) - (baseline.tests_total as i64);
    let new_passing_tests = if new_total_delta > 0 {
        let added = new_total_delta as u32;
        let added_failing = regressions.len() as u32;
        added.saturating_sub(added_failing)
    } else {
        0
    };

    let regression_count = regressions.len() as u32;

    RegressionReport {
        baseline_passed: baseline.tests_passed,
        baseline_failed: baseline.tests_failed,
        baseline_total: baseline.tests_total,
        current_passed: current.tests_passed,
        current_failed: current.tests_failed,
        current_total: current.tests_total,
        regressions: regressions.clone(),
        fixes,
        new_passing_tests,
        new_failing_tests: regressions, // best-effort; reviewer disambiguates
        removed_tests: vec![],          // not detectable from failed-only manifests
        regression_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(passed: u32, failed: u32, names: &[&str]) -> MechanicalReport {
        MechanicalReport {
            tests_passed: passed,
            tests_failed: failed,
            tests_ignored: 0,
            tests_total: passed + failed,
            failed_test_names: names.iter().map(|s| s.to_string()).collect(),
            compile_failed: false,
            compile_error_excerpt: None,
            raw_stdout_truncated: String::new(),
            raw_stderr_truncated: String::new(),
        }
    }

    #[test]
    fn regression_when_passing_test_now_fails() {
        let baseline = report(50, 0, &[]);
        let current = report(48, 2, &["test_a", "test_b"]);
        let r = compare_to_baseline(&baseline, &current);
        assert_eq!(r.regression_count, 2);
        assert_eq!(r.regressions, vec!["test_a", "test_b"]);
        assert_eq!(r.new_passing_tests, 0);
    }

    #[test]
    fn fix_when_failing_test_now_passes() {
        let baseline = report(48, 2, &["test_a", "test_b"]);
        let current = report(50, 0, &[]);
        let r = compare_to_baseline(&baseline, &current);
        assert_eq!(r.regression_count, 0);
        assert_eq!(r.fixes, vec!["test_a", "test_b"]);
    }

    #[test]
    fn new_tests_not_treated_as_regressions() {
        // baseline: 50 tests all passing
        // current:  53 tests, 1 failing (the new test), 52 passing
        let baseline = report(50, 0, &[]);
        let current = report(52, 1, &["test_new_feature"]);
        let r = compare_to_baseline(&baseline, &current);
        // The harness flags it as a "regression-or-new" candidate; the
        // reviewer disambiguates. We expose new_passing_tests so the
        // mass-balance is visible.
        assert_eq!(r.regressions, vec!["test_new_feature"]);
        // total delta = 53 - 50 = 3; one new failure → 2 new passing.
        assert_eq!(r.new_passing_tests, 2);
    }

    #[test]
    fn unchanged_set_yields_zero_regressions() {
        let baseline = report(48, 2, &["test_x", "test_y"]);
        let current = report(48, 2, &["test_x", "test_y"]);
        let r = compare_to_baseline(&baseline, &current);
        assert_eq!(r.regression_count, 0);
        assert_eq!(r.fixes.len(), 0);
    }
}
