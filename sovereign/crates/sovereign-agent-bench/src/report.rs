//! `BenchReport` JSON shape + text rollup.

use serde::{Deserialize, Serialize};

use crate::scoring::{ProblemScore, ProblemTrialDetail, RegressionDelta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub agent: String,
    pub model: String,
    pub judge_model: String,
    pub judge_trials: u8,
    /// Number of independent agent runs per problem this report
    /// covers (`--trials` flag). Default 1; >1 means
    /// `per_problem_trials` carries the per-trial breakdown.
    #[serde(default = "default_run_trials")]
    pub run_trials: u8,
    pub started_at: String,  // RFC3339
    pub finished_at: String, // RFC3339
    pub per_problem: Vec<ProblemScore>,
    /// Per-trial detail when `run_trials > 1`. Each entry's
    /// `problem_id` matches a `per_problem` entry; the headline
    /// `ProblemScore` is the trial closest to the mean (per
    /// `representative_index`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_problem_trials: Vec<ProblemTrialDetail>,
    pub grand_total: u16,
    pub max_total: u16,
    pub regression: Option<RegressionDelta>,
}

fn default_run_trials() -> u8 {
    1
}

impl BenchReport {
    pub fn compute_grand_total(problems: &[ProblemScore]) -> u16 {
        problems.iter().map(|p| p.total as u16).sum::<u16>().min(72)
    }

    /// Text rollup — concise, one line per problem + grand total.
    pub fn text_rollup(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Agent-bench report — agent={} model={} judge={} judge_trials={} run_trials={}\n",
            self.agent, self.model, self.judge_model, self.judge_trials, self.run_trials
        ));
        out.push_str(&format!(
            "Started {} → Finished {}\n",
            self.started_at, self.finished_at
        ));
        out.push('\n');
        for p in &self.per_problem {
            out.push_str(&format!(
                "  {id:<32} {a}/{b}/{c} = {total}/9   exit={exit:<16} tokens(out)={tok:>6} wall={wall}ms{partial}\n",
                id = p.problem_id,
                a = p.dim_a.raw,
                b = p.dim_b.raw,
                c = p.dim_c.raw,
                total = p.total,
                exit = p.exit_reason.id(),
                tok = p.tokens.output,
                wall = p.wall_ms,
                partial = if p.is_partial { "  (partial)" } else { "" },
            ));
            // Multi-trial supplement: per-trial totals + mean ± stdev
            // + exit_reason histogram. Surfaces the "lucky vs stable"
            // signal the operator needs to read variance honestly.
            if let Some(d) = self
                .per_problem_trials
                .iter()
                .find(|d| d.problem_id == p.problem_id)
            {
                let per_trial: Vec<String> =
                    d.per_trial.iter().map(|t| t.total.to_string()).collect();
                let mut exit_counts: std::collections::BTreeMap<&str, u32> =
                    std::collections::BTreeMap::new();
                for t in &d.per_trial {
                    *exit_counts.entry(t.exit_reason.id()).or_insert(0) += 1;
                }
                let exit_mix: Vec<String> = exit_counts
                    .iter()
                    .map(|(k, v)| format!("{k}×{v}"))
                    .collect();
                out.push_str(&format!(
                    "  {:<32} N={n} mean={mean:.2}±{stdev:.2} totals=({tot}) exit_mix={mix}\n",
                    "",
                    n = d.n,
                    mean = d.mean_total,
                    stdev = d.stdev_total,
                    tot = per_trial.join(","),
                    mix = exit_mix.join(","),
                ));
            }
        }
        out.push('\n');
        out.push_str(&format!(
            "Grand total: {} / {}\n",
            self.grand_total, self.max_total
        ));
        if let Some(r) = &self.regression {
            out.push_str(&format!(
                "Regression vs latest.json: delta={} (regressed={})\n",
                r.delta, r.regressed
            ));
            for pd in &r.per_problem {
                if pd.delta != 0 {
                    out.push_str(&format!(
                        "  {:<32} {} → {} (delta {:+})\n",
                        pd.problem_id, pd.prior_total, pd.current_total, pd.delta
                    ));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{ExitReason, TokenCounts};
    use crate::scoring::dim_from_auto;

    fn fake_score(id: &str, a: u8, b: u8, c: u8) -> ProblemScore {
        ProblemScore {
            problem_id: id.into(),
            dim_a: dim_from_auto(1.0, a, true),
            dim_b: dim_from_auto(1.0, b, true),
            dim_c: dim_from_auto(1.0, c, true),
            total: a + b + c,
            exit_reason: ExitReason::Completed,
            tokens: TokenCounts {
                input: 100,
                output: 50,
            },
            wall_ms: 1000,
            tool_calls: vec![],
            witness_summary: None,
            is_partial: false,
        }
    }

    #[test]
    fn compute_grand_total_sums_per_problem() {
        let p = vec![fake_score("a", 3, 2, 1), fake_score("b", 2, 2, 2)];
        assert_eq!(BenchReport::compute_grand_total(&p), 12);
    }

    #[test]
    fn compute_grand_total_caps_at_72() {
        let p = (0..10)
            .map(|i| fake_score(&format!("p{i}"), 3, 3, 3))
            .collect::<Vec<_>>();
        // 10 × 9 = 90, capped to 72
        assert_eq!(BenchReport::compute_grand_total(&p), 72);
    }

    #[test]
    fn text_rollup_includes_grand_total_and_per_problem() {
        let r = BenchReport {
            agent: "pi".into(),
            model: "commonwealth/coder".into(),
            judge_model: "commonwealth/coder".into(),
            judge_trials: 3,
            run_trials: 1,
            started_at: "2026-05-20T12:00:00Z".into(),
            finished_at: "2026-05-20T12:10:00Z".into(),
            per_problem: vec![fake_score("3.2-lights-out", 3, 2, 2)],
            per_problem_trials: vec![],
            grand_total: 7,
            max_total: 9,
            regression: None,
        };
        let s = r.text_rollup();
        assert!(s.contains("3.2-lights-out"));
        assert!(s.contains("Grand total: 7 / 9"));
        assert!(s.contains("commonwealth/coder"));
        assert!(s.contains("run_trials=1"));
        assert!(!s.contains("mean="));
    }

    #[test]
    fn text_rollup_surfaces_multi_trial_variance() {
        use crate::scoring::ProblemTrialDetail;
        let trials = vec![
            fake_score("2.2-x", 3, 2, 3),
            fake_score("2.2-x", 0, 1, 0),
            fake_score("2.2-x", 3, 2, 2),
        ];
        let detail = ProblemTrialDetail::from_trials("2.2-x", &trials);
        let r = BenchReport {
            agent: "pi".into(),
            model: "commonwealth/primary".into(),
            judge_model: "commonwealth/primary".into(),
            judge_trials: 1,
            run_trials: 3,
            started_at: "2026-05-21T20:00:00Z".into(),
            finished_at: "2026-05-21T21:00:00Z".into(),
            per_problem: vec![fake_score("2.2-x", 3, 2, 2)],
            per_problem_trials: vec![detail],
            grand_total: 7,
            max_total: 9,
            regression: None,
        };
        let s = r.text_rollup();
        assert!(s.contains("run_trials=3"));
        assert!(s.contains("N=3"));
        assert!(s.contains("mean="));
        assert!(s.contains("totals=(8,1,7)"));
        assert!(s.contains("exit_mix=completed×3"));
    }
}
