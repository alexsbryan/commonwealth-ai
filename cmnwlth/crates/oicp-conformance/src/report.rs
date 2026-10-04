// SPDX-License-Identifier: AGPL-3.0-or-later
//! The check-result model, the report artifact, and the baseline-diff gate.
//!
//! The gate idiom mirrors `sovereign bench gate` but is self-contained (no
//! dependency on it): a run emits a `Report`; `--baseline` diffs it against a
//! prior report and a **regression** (`pass` → `fail`/`skip`) fails the run.
//! First run with no baseline passes; `--update-baseline` writes the new one.

use serde::{Deserialize, Serialize};

/// Spec obligation level of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// A MUST in the spec — failure means non-conformance.
    Must,
    /// A SHOULD — failure is a warning, not a hard non-conformance.
    Should,
    /// Gated on an advertised feature — `Skip` when the feature is absent.
    Feature,
}

/// Outcome of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    /// Not applicable (feature not advertised, or a prerequisite like
    /// `--fixture-recipe` absent). A skip is NOT a failure.
    Skip,
}

/// One conformance check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Stable dotted id, e.g. `manifest.features`.
    pub id: String,
    pub level: Level,
    pub status: CheckStatus,
    /// Human-readable one-liner: why it passed/failed, or why it was skipped.
    pub detail: String,
}

impl Check {
    pub fn pass(id: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level,
            status: CheckStatus::Pass,
            detail: detail.into(),
        }
    }
    pub fn fail(id: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level,
            status: CheckStatus::Fail,
            detail: detail.into(),
        }
    }
    pub fn skip(id: &str, level: Level, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level,
            status: CheckStatus::Skip,
            detail: detail.into(),
        }
    }
}

/// The full run artifact. Deliberately flat + serde-stable so a `--baseline`
/// from an older build still diffs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub host: String,
    pub oicp_version: String,
    pub checks: Vec<Check>,
}

impl Report {
    /// A run is conformant when no `must`-level check FAILED. `should` failures
    /// and skips do not sink the run (they surface in the summary).
    pub fn is_conformant(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|c| c.level == Level::Must && c.status == CheckStatus::Fail)
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let mut pass = 0;
        let mut fail = 0;
        let mut skip = 0;
        for c in &self.checks {
            match c.status {
                CheckStatus::Pass => pass += 1,
                CheckStatus::Fail => fail += 1,
                CheckStatus::Skip => skip += 1,
            }
        }
        (pass, fail, skip)
    }
}

/// A single regression relative to a baseline: a check that used to pass and
/// now doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regression {
    pub id: String,
    pub was: CheckStatus,
    pub now: CheckStatus,
}

/// Diff `current` against `baseline`. A regression is a check that was `Pass`
/// in the baseline and is now `Fail` or `Skip`. New checks (absent from the
/// baseline) are never regressions; a check that DISAPPEARS is reported so a
/// silently-dropped check can't hide.
pub fn regressions(baseline: &Report, current: &Report) -> Vec<Regression> {
    let mut out = Vec::new();
    for base in &baseline.checks {
        if base.status != CheckStatus::Pass {
            continue;
        }
        match current.checks.iter().find(|c| c.id == base.id) {
            Some(cur) if cur.status != CheckStatus::Pass => out.push(Regression {
                id: base.id.clone(),
                was: base.status,
                now: cur.status,
            }),
            None => out.push(Regression {
                id: base.id.clone(),
                was: base.status,
                now: CheckStatus::Skip, // treat "vanished" as no-longer-passing
            }),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(checks: Vec<Check>) -> Report {
        Report {
            host: "h".into(),
            oicp_version: "0.4.0".into(),
            checks,
        }
    }

    #[test]
    fn conformance_ignores_should_and_skip() {
        let rep = r(vec![
            Check::pass("a", Level::Must, ""),
            Check::fail("b", Level::Should, ""),
            Check::skip("c", Level::Feature, ""),
        ]);
        assert!(rep.is_conformant());
        assert_eq!(rep.counts(), (1, 1, 1));
    }

    #[test]
    fn a_failed_must_sinks_conformance() {
        let rep = r(vec![Check::fail("a", Level::Must, "")]);
        assert!(!rep.is_conformant());
    }

    #[test]
    fn regression_is_pass_to_not_pass() {
        let base = r(vec![
            Check::pass("a", Level::Must, ""),
            Check::pass("b", Level::Feature, ""),
            Check::fail("c", Level::Should, ""), // was already failing — not a regression
        ]);
        let cur = r(vec![
            Check::pass("a", Level::Must, ""),
            Check::skip("b", Level::Feature, ""), // pass → skip = regression
            Check::fail("c", Level::Should, ""),
        ]);
        let regs = regressions(&base, &cur);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].id, "b");
        assert_eq!(regs[0].was, CheckStatus::Pass);
        assert_eq!(regs[0].now, CheckStatus::Skip);
    }

    #[test]
    fn a_vanished_passing_check_is_a_regression() {
        let base = r(vec![Check::pass("a", Level::Must, "")]);
        let cur = r(vec![]);
        let regs = regressions(&base, &cur);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].id, "a");
    }

    #[test]
    fn first_run_no_regressions_against_itself() {
        let rep = r(vec![
            Check::pass("a", Level::Must, ""),
            Check::skip("b", Level::Feature, ""),
        ]);
        assert!(regressions(&rep, &rep).is_empty());
    }
}
