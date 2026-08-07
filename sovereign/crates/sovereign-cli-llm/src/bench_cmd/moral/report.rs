// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moral-lane report shapes — a binding over the shared rubric core.
//!
//! The formulas (weighted score, Wilson CI, aggregation, the
//! disjoint-CI significance rule) and the calibration gate live in
//! [`crate::bench_cmd::rubric`] and are NOT duplicated here. What is
//! moral-specific and therefore stays: the scenario's provenance
//! fields (`role_domain`, `dilemma_source`), the run header, and the
//! per-scenario line format.
//!
//! The JSON wire shape is unchanged by the extraction — field names
//! and order are exactly what they were when the lane shipped, so
//! reports banked before the split still load.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::scenarios::Scenario;
use crate::bench_cmd::rubric::report::{self as shared, RubricRun};
use crate::bench_cmd::rubric::score::{score_item, Aggregate, CriterionOutcome, RubricItem};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub scenario_id: String,
    pub role_domain: String,
    pub dilemma_source: String,
    /// 0–100, `None` when every criterion was could-not-judge.
    pub score: Option<f64>,
    pub criteria: Vec<CriterionOutcome>,
    pub could_not_judge: usize,
    /// Full model output, kept for audit — every per-criterion
    /// verdict must be checkable against the text it judged.
    pub response: String,
    pub gen_ms: u64,
    pub judge_ms_total: u64,
}

impl RubricItem for ScenarioReport {
    fn id(&self) -> &str {
        &self.scenario_id
    }
    fn score(&self) -> Option<f64> {
        self.score
    }
    fn criteria(&self) -> &[CriterionOutcome] {
        &self.criteria
    }
    fn group(&self) -> Option<&str> {
        Some(&self.role_domain)
    }
}

pub fn build_scenario_report(
    scenario: &Scenario,
    outcomes: Vec<CriterionOutcome>,
    response: String,
    gen_ms: u64,
    judge_ms_total: u64,
) -> ScenarioReport {
    let could_not_judge = outcomes
        .iter()
        .filter(|o| o.verdict.verdict.is_none())
        .count();
    ScenarioReport {
        scenario_id: scenario.scenario.id.clone(),
        role_domain: scenario.scenario.role_domain.clone(),
        dilemma_source: scenario.scenario.dilemma_source.clone(),
        score: score_item(&outcomes),
        criteria: outcomes,
        could_not_judge,
        response,
        gen_ms,
        judge_ms_total,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoralEvalRun {
    pub started_at_unix: i64,
    pub chat_model: String,
    pub judge_model: String,
    pub judge_trials: u8,
    pub max_tokens: u32,
    pub scenarios: Vec<ScenarioReport>,
    pub aggregate: Aggregate,
}

impl RubricRun for MoralEvalRun {
    type Item = ScenarioReport;
    fn items(&self) -> &[ScenarioReport] {
        &self.scenarios
    }
    fn subject_model(&self) -> &str {
        &self.chat_model
    }
    fn judge_model(&self) -> &str {
        &self.judge_model
    }
    fn judge_trials(&self) -> u8 {
        self.judge_trials
    }
    fn aggregate(&self) -> &Aggregate {
        &self.aggregate
    }
}

pub fn aggregate(scenarios: &[ScenarioReport]) -> Aggregate {
    crate::bench_cmd::rubric::score::aggregate(scenarios)
}

pub fn write_json_report(path: &Path, run: &MoralEvalRun) -> std::io::Result<()> {
    shared::write_json_report(path, run)
}

pub fn load_report(path: &Path) -> std::io::Result<MoralEvalRun> {
    shared::load_report(path)
}

pub fn print_diff(baseline: &MoralEvalRun, current: &MoralEvalRun) {
    shared::print_diff(baseline, current)
}

pub fn print_text_report(run: &MoralEvalRun) {
    println!("bench moral — {} scenarios", run.scenarios.len());
    println!("chat model:   {}", run.chat_model);
    println!(
        "judge model:  {}  (trials per criterion: {})",
        run.judge_model, run.judge_trials
    );
    println!("==========================================");
    for s in &run.scenarios {
        let score = match s.score {
            Some(v) => format!("{v:5.1}"),
            None => "  n/a".to_string(),
        };
        let cnj = if s.could_not_judge > 0 {
            format!("  [{} could-not-judge]", s.could_not_judge)
        } else {
            String::new()
        };
        println!(
            "{score}  {}  ({}, {})  gen {:.1}s  judge {:.1}s{cnj}",
            s.scenario_id,
            s.role_domain,
            s.dilemma_source,
            s.gen_ms as f64 / 1000.0,
            s.judge_ms_total as f64 / 1000.0,
        );
    }
    let a = &run.aggregate;
    shared::print_score_summary(a);
    shared::print_dimension_table(a);
    shared::print_group_table(a, "role domain");
    shared::print_could_not_judge(a);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_cmd::rubric::judge::{CriterionVerdict, Judgement};

    fn outcome(id: &str, dim: &str, weight: i32, verdict: Option<Judgement>) -> CriterionOutcome {
        CriterionOutcome {
            criterion_id: id.into(),
            dimension: dim.into(),
            weight,
            verdict: CriterionVerdict {
                verdict,
                evidence: String::new(),
                trials_yes: matches!(verdict, Some(Judgement::Yes)) as u32,
                trials_no: matches!(verdict, Some(Judgement::No)) as u32,
                trials_failed: verdict.is_none() as u32,
            },
        }
    }

    fn scenario_report(id: &str, role: &str, outcomes: Vec<CriterionOutcome>) -> ScenarioReport {
        let cnj = outcomes
            .iter()
            .filter(|o| o.verdict.verdict.is_none())
            .count();
        ScenarioReport {
            scenario_id: id.into(),
            role_domain: role.into(),
            dilemma_source: "daily_dilemmas".into(),
            score: score_item(&outcomes),
            criteria: outcomes,
            could_not_judge: cnj,
            response: String::new(),
            gen_ms: 0,
            judge_ms_total: 0,
        }
    }

    #[test]
    fn report_json_round_trips() {
        let run = MoralEvalRun {
            started_at_unix: 0,
            chat_model: "gemma".into(),
            judge_model: "qwen".into(),
            judge_trials: 3,
            max_tokens: 2000,
            scenarios: vec![scenario_report(
                "s1",
                "ai_agent",
                vec![outcome("a", "identifying", 2, Some(Judgement::Yes))],
            )],
            aggregate: Aggregate::default(),
        };
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.json");
        write_json_report(&p, &run).unwrap();
        let loaded = load_report(&p).unwrap();
        assert_eq!(loaded.chat_model, "gemma");
        assert_eq!(loaded.scenarios[0].criteria.len(), 1);
    }

    /// The wire shape is a contract: reports banked before the rubric
    /// extraction (P1) must still load, and a report written after it
    /// must still carry the same top-level keys in the same order.
    /// A silent key rename would strand every baseline on disk.
    #[test]
    fn json_keys_survive_the_rubric_extraction() {
        let run = MoralEvalRun {
            started_at_unix: 0,
            chat_model: "gemma".into(),
            judge_model: "qwen".into(),
            judge_trials: 1,
            max_tokens: 2000,
            scenarios: vec![scenario_report(
                "s1",
                "ai_agent",
                vec![outcome("a", "identifying", 2, Some(Judgement::Yes))],
            )],
            aggregate: aggregate(&[scenario_report(
                "s1",
                "ai_agent",
                vec![outcome("a", "identifying", 2, Some(Judgement::Yes))],
            )]),
        };
        let v = serde_json::to_value(&run).unwrap();
        let top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            top,
            vec![
                "started_at_unix",
                "chat_model",
                "judge_model",
                "judge_trials",
                "max_tokens",
                "scenarios",
                "aggregate"
            ]
        );
        let s = &v["scenarios"][0];
        let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "scenario_id",
                "role_domain",
                "dilemma_source",
                "score",
                "criteria",
                "could_not_judge",
                "response",
                "gen_ms",
                "judge_ms_total"
            ]
        );
        // The criterion verdict stays flattened onto the outcome.
        let c = &v["scenarios"][0]["criteria"][0];
        for k in [
            "criterion_id",
            "dimension",
            "weight",
            "verdict",
            "evidence",
            "trials_yes",
        ] {
            assert!(c.get(k).is_some(), "criterion outcome lost key `{k}`");
        }
        // And the aggregate keeps the axis name the reports use.
        assert!(v["aggregate"].get("by_role_domain").is_some());
        assert!(v["aggregate"].get("by_dimension").is_some());
    }
}
