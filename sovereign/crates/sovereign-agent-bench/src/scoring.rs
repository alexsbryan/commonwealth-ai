//! Per-dimension → per-problem → grand-total aggregation, plus
//! regression delta against a prior `BenchReport`.

use serde::{Deserialize, Serialize};

use crate::judge_multi::MultiTrialOutcome;
use crate::runner::{ExitReason, TokenCounts, ToolCallRecord};
use crate::witness::AutoWitnessOutcome;

/// Where a dimension's score came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScoreSource {
    /// Auto-witness only (cargo / go / vitest / pytest pass fraction).
    Auto {
        pass_fraction: f64,
        verify_exit_ok: bool,
    },
    /// LLM judge (N trials).
    Judge { trials: u8, coverage_mean: f64 },
    /// `HybridAutoFloor`: the auto-witness sets a floor; the judge can
    /// lift but not lower.
    Hybrid {
        auto_score: u8,
        judge_score: u8,
        coverage_mean: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionScore {
    pub raw: u8, // 0..=3
    pub source: ScoreSource,
    /// Majority anchor across judge trials (if any).
    pub anchor_majority: Option<u8>,
    /// Per-trial anchor sequence (judge-mode only).
    pub anchor_per_trial: Vec<u8>,
    /// Whether the majority was reached.
    pub majority_reached: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessSummary {
    pub verify_exit_ok: bool,
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    pub pass_fraction: f64,
}

impl WitnessSummary {
    pub fn from_outcome(o: &AutoWitnessOutcome) -> Self {
        Self {
            verify_exit_ok: o.verify_exit_ok,
            passed: o.parsed.passed,
            failed: o.parsed.failed,
            total: o.parsed.total,
            pass_fraction: o.parsed.pass_fraction(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemScore {
    pub problem_id: String,
    pub dim_a: DimensionScore,
    pub dim_b: DimensionScore,
    pub dim_c: DimensionScore,
    pub total: u8, // 0..=9
    pub exit_reason: ExitReason,
    pub tokens: TokenCounts,
    pub wall_ms: u64,
    pub tool_calls: Vec<ToolCallRecord>,
    pub witness_summary: Option<WitnessSummary>,
    /// True when `exit_reason != Completed`. Surfaces partial-credit
    /// runs in operator views.
    pub is_partial: bool,
}

impl ProblemScore {
    pub fn compute_total(dim_a: u8, dim_b: u8, dim_c: u8) -> u8 {
        (dim_a + dim_b + dim_c).min(9)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionDelta {
    pub prior_grand_total: u16,
    pub current_grand_total: u16,
    pub delta: i32,
    pub per_problem: Vec<ProblemDelta>,
    pub regressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemDelta {
    pub problem_id: String,
    pub prior_total: u8,
    pub current_total: u8,
    pub delta: i32,
}

/// Build a `DimensionScore` for an auto-witness dimension.
pub fn dim_from_auto(pass_fraction: f64, bucketed: u8, verify_exit_ok: bool) -> DimensionScore {
    DimensionScore {
        raw: bucketed,
        source: ScoreSource::Auto {
            pass_fraction,
            verify_exit_ok,
        },
        anchor_majority: None,
        anchor_per_trial: vec![],
        majority_reached: None,
    }
}

/// Build a `DimensionScore` for a pure judge dimension.
pub fn dim_from_judge(judge: &MultiTrialOutcome) -> DimensionScore {
    DimensionScore {
        raw: judge.majority_anchor,
        source: ScoreSource::Judge {
            trials: judge.trials.len() as u8,
            coverage_mean: judge.coverage_mean,
        },
        anchor_majority: Some(judge.majority_anchor),
        anchor_per_trial: judge.trials.iter().map(|t| t.anchor).collect(),
        majority_reached: Some(judge.majority_reached),
    }
}

/// Build a `DimensionScore` for a hybrid auto-floor dimension. The
/// auto-witness provides the lower bound; the judge can lift the score
/// but never lower it (a beautiful incorrect implementation should not
/// outscore a working one).
pub fn dim_from_hybrid(auto_score: u8, judge: &MultiTrialOutcome) -> DimensionScore {
    let combined = judge.majority_anchor.max(auto_score);
    DimensionScore {
        raw: combined,
        source: ScoreSource::Hybrid {
            auto_score,
            judge_score: judge.majority_anchor,
            coverage_mean: judge.coverage_mean,
        },
        anchor_majority: Some(judge.majority_anchor),
        anchor_per_trial: judge.trials.iter().map(|t| t.anchor).collect(),
        majority_reached: Some(judge.majority_reached),
    }
}

/// Grand-total regression compare against a prior report. `threshold`
/// is the minimum negative delta that counts as a regression (default
/// 1: a single problem dropping a point flags the run).
pub fn compute_regression(
    current: &[ProblemScore],
    prior: &[ProblemScore],
    threshold: i32,
) -> RegressionDelta {
    let mut per_problem: Vec<ProblemDelta> = Vec::new();
    let mut prior_total: u16 = 0;
    let mut current_total: u16 = 0;
    for p in prior {
        prior_total = prior_total.saturating_add(p.total as u16);
    }
    for c in current {
        current_total = current_total.saturating_add(c.total as u16);
        let prior_match = prior.iter().find(|p| p.problem_id == c.problem_id);
        if let Some(prior_p) = prior_match {
            per_problem.push(ProblemDelta {
                problem_id: c.problem_id.clone(),
                prior_total: prior_p.total,
                current_total: c.total,
                delta: c.total as i32 - prior_p.total as i32,
            });
        } else {
            // New problem this run — no delta to compute, but record
            // for visibility.
            per_problem.push(ProblemDelta {
                problem_id: c.problem_id.clone(),
                prior_total: 0,
                current_total: c.total,
                delta: c.total as i32,
            });
        }
    }
    let delta = current_total as i32 - prior_total as i32;
    let regressed = per_problem.iter().any(|p| p.delta <= -threshold);
    RegressionDelta {
        prior_grand_total: prior_total,
        current_grand_total: current_total,
        delta,
        per_problem,
        regressed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::JudgeTrialOutcome;

    fn outcome(anchors: &[u8]) -> MultiTrialOutcome {
        let trials = anchors
            .iter()
            .map(|a| JudgeTrialOutcome {
                anchor: *a,
                rationale: String::new(),
            })
            .collect::<Vec<_>>();
        crate::judge_multi::aggregate(trials, anchors.len() as u8)
    }

    #[test]
    fn dim_from_hybrid_takes_max() {
        let judge = outcome(&[2, 2, 2]);
        let d = dim_from_hybrid(3, &judge);
        assert_eq!(d.raw, 3, "auto floor must hold when it's higher");

        let judge2 = outcome(&[3, 3, 3]);
        let d2 = dim_from_hybrid(1, &judge2);
        assert_eq!(d2.raw, 3, "judge must lift when higher");
    }

    #[test]
    fn dim_from_judge_uses_majority_anchor() {
        let judge = outcome(&[1, 1, 2]);
        let d = dim_from_judge(&judge);
        assert_eq!(d.raw, 1);
        assert!(d.majority_reached.unwrap_or(false));
        assert_eq!(d.anchor_per_trial, vec![1, 1, 2]);
    }

    #[test]
    fn compute_regression_flags_drop() {
        let prior = vec![ProblemScore {
            problem_id: "3.2".into(),
            dim_a: dim_from_auto(1.0, 3, true),
            dim_b: dim_from_auto(1.0, 2, true),
            dim_c: dim_from_auto(1.0, 2, true),
            total: 7,
            exit_reason: ExitReason::Completed,
            tokens: TokenCounts::default(),
            wall_ms: 0,
            tool_calls: vec![],
            witness_summary: None,
            is_partial: false,
        }];
        let curr = vec![ProblemScore {
            problem_id: "3.2".into(),
            dim_a: dim_from_auto(0.5, 1, true),
            dim_b: dim_from_auto(1.0, 2, true),
            dim_c: dim_from_auto(1.0, 2, true),
            total: 5,
            exit_reason: ExitReason::Completed,
            tokens: TokenCounts::default(),
            wall_ms: 0,
            tool_calls: vec![],
            witness_summary: None,
            is_partial: false,
        }];
        let r = compute_regression(&curr, &prior, 1);
        assert!(r.regressed);
        assert_eq!(r.delta, -2);
        assert_eq!(r.per_problem.len(), 1);
        assert_eq!(r.per_problem[0].delta, -2);
    }

    #[test]
    fn compute_regression_no_drop_within_threshold() {
        let prior = vec![ProblemScore {
            problem_id: "3.2".into(),
            dim_a: dim_from_auto(1.0, 3, true),
            dim_b: dim_from_auto(1.0, 3, true),
            dim_c: dim_from_auto(1.0, 3, true),
            total: 9,
            exit_reason: ExitReason::Completed,
            tokens: TokenCounts::default(),
            wall_ms: 0,
            tool_calls: vec![],
            witness_summary: None,
            is_partial: false,
        }];
        let curr = prior.clone();
        let r = compute_regression(&curr, &prior, 1);
        assert!(!r.regressed);
        assert_eq!(r.delta, 0);
    }

    #[test]
    fn compute_regression_handles_new_problem() {
        let prior: Vec<ProblemScore> = vec![];
        let curr = vec![ProblemScore {
            problem_id: "3.2".into(),
            dim_a: dim_from_auto(1.0, 3, true),
            dim_b: dim_from_auto(1.0, 2, true),
            dim_c: dim_from_auto(1.0, 2, true),
            total: 7,
            exit_reason: ExitReason::Completed,
            tokens: TokenCounts::default(),
            wall_ms: 0,
            tool_calls: vec![],
            witness_summary: None,
            is_partial: false,
        }];
        let r = compute_regression(&curr, &prior, 1);
        assert!(!r.regressed);
        assert_eq!(r.delta, 7);
        assert_eq!(r.per_problem[0].prior_total, 0);
    }
}
