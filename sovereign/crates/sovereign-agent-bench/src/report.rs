//! `BenchReport` JSON shape + text rollup.

use serde::{Deserialize, Serialize};

use crate::scoring::{ProblemScore, RegressionDelta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub agent: String,
    pub model: String,
    pub judge_model: String,
    pub judge_trials: u8,
    pub started_at: String,  // RFC3339
    pub finished_at: String, // RFC3339
    pub per_problem: Vec<ProblemScore>,
    pub grand_total: u16,
    pub max_total: u16,
    pub regression: Option<RegressionDelta>,
}

impl BenchReport {
    pub fn compute_grand_total(problems: &[ProblemScore]) -> u16 {
        problems
            .iter()
            .map(|p| p.total as u16)
            .sum::<u16>()
            .min(72)
    }

    /// Text rollup — concise, one line per problem + grand total.
    pub fn text_rollup(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Agent-bench report — agent={} model={} judge={} trials={}\n",
            self.agent, self.model, self.judge_model, self.judge_trials
        ));
        out.push_str(&format!(
            "Started {} → Finished {}\n",
            self.started_at, self.finished_at
        ));
        out.push_str("\n");
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
        }
        out.push_str("\n");
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
            started_at: "2026-05-20T12:00:00Z".into(),
            finished_at: "2026-05-20T12:10:00Z".into(),
            per_problem: vec![fake_score("3.2-lights-out", 3, 2, 2)],
            grand_total: 7,
            max_total: 9,
            regression: None,
        };
        let s = r.text_rollup();
        assert!(s.contains("3.2-lights-out"));
        assert!(s.contains("Grand total: 7 / 9"));
        assert!(s.contains("commonwealth/coder"));
    }
}
