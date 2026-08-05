// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moral-eval live runner.
//!
//! Deliberately simpler than `voice_eval`'s: no Runtime, no skills,
//! no seed memories. A dilemma is a single user turn against a bare
//! chat model (matching the MoReBench reference protocol: one user
//! message, temperature 0), and the judge is a second pinned model
//! reached through the same daemon. Both models are addressed by id
//! via `RemoteApiProvider`, so a Gemma-vs-Qwen A/B is two runs with
//! different `--chat-model` and the SAME `--judge-model`.

use std::time::Instant;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_inference::remote::RemoteApiProvider;

use super::report::{self, MoralEvalRun, ScenarioReport};
use super::scenarios::Scenario;
use crate::bench_cmd::rubric::judge;
use crate::bench_cmd::rubric::score::CriterionOutcome;

pub struct RunOptions {
    pub daemon_base: String,
    pub chat_model: String,
    pub judge_model: String,
    pub judge_trials: u8,
    pub max_tokens: u32,
}

/// Generation request: bare user prompt, temperature 0 (the
/// reference protocol), thinking left at the model's default — the
/// judge scores the full visible output, reasoning included, since
/// process is what this bench measures.
fn generation_request(dilemma: &str, max_tokens: u32) -> CompletionRequest {
    CompletionRequest {
        prompt: dilemma.to_string(),
        preferred_speed: Speed::Slow,
        max_tokens: Some(max_tokens as usize),
        temperature: Some(0.0),
        ..Default::default()
    }
}

pub async fn run(scenarios: &[Scenario], opts: &RunOptions) -> Result<MoralEvalRun, String> {
    let v1 = format!("{}/v1", opts.daemon_base.trim_end_matches('/'));
    let chat = RemoteApiProvider::new(&v1, None, &opts.chat_model, 16384);
    let judge_provider = RemoteApiProvider::new(&v1, None, &opts.judge_model, 16384);

    if opts.chat_model == opts.judge_model {
        eprintln!(
            "bench moral: WARNING — judge model equals chat model ({}); self-judging \
             conflates the rater with the rated. Pin --judge-model for comparable scores.",
            opts.chat_model
        );
    }

    let mut reports: Vec<ScenarioReport> = Vec::with_capacity(scenarios.len());
    for (idx, scenario) in scenarios.iter().enumerate() {
        eprintln!(
            "bench moral: [{}/{}] {} ({} criteria)",
            idx + 1,
            scenarios.len(),
            scenario.scenario.id,
            scenario.criteria.len()
        );

        let gen_started = Instant::now();
        let response = match chat
            .complete(&generation_request(&scenario.dilemma.prompt, opts.max_tokens))
            .await
        {
            Ok(r) => r.text,
            Err(e) => {
                // A failed generation is a failed scenario, loudly:
                // every criterion becomes could-not-judge, which
                // counts toward the degraded gate rather than being
                // dropped from the run.
                eprintln!("  generation FAILED: {e}");
                let outcomes: Vec<CriterionOutcome> = scenario
                    .criteria
                    .iter()
                    .map(|c| CriterionOutcome {
                        criterion_id: c.id.clone(),
                        dimension: c.dimension.as_str().to_string(),
                        weight: c.weight,
                        verdict: judge::CriterionVerdict {
                            verdict: None,
                            evidence: format!("generation failed: {e}"),
                            trials_yes: 0,
                            trials_no: 0,
                            trials_failed: opts.judge_trials.max(1) as u32,
                        },
                    })
                    .collect();
                reports.push(report::build_scenario_report(
                    scenario,
                    outcomes,
                    format!("[generation failed] {e}"),
                    gen_started.elapsed().as_millis() as u64,
                    0,
                ));
                continue;
            }
        };
        let gen_ms = gen_started.elapsed().as_millis() as u64;

        let judge_started = Instant::now();
        let mut outcomes = Vec::with_capacity(scenario.criteria.len());
        for criterion in &scenario.criteria {
            let verdict = judge::judge_criterion(
                &judge_provider,
                &response,
                &criterion.text,
                Some(&opts.judge_model),
                opts.judge_trials,
            )
            .await;
            outcomes.push(CriterionOutcome {
                criterion_id: criterion.id.clone(),
                dimension: criterion.dimension.as_str().to_string(),
                weight: criterion.weight,
                verdict,
            });
            eprint!(".");
        }
        eprintln!();
        let judge_ms_total = judge_started.elapsed().as_millis() as u64;

        let sr = report::build_scenario_report(scenario, outcomes, response, gen_ms, judge_ms_total);
        if let Some(score) = sr.score {
            eprintln!(
                "  score {score:.1}  (gen {:.1}s, judge {:.1}s)",
                gen_ms as f64 / 1000.0,
                judge_ms_total as f64 / 1000.0
            );
        } else {
            eprintln!("  score n/a — every criterion could-not-judge");
        }
        reports.push(sr);
    }

    let aggregate = report::aggregate(&reports);
    Ok(MoralEvalRun {
        started_at_unix: chrono::Utc::now().timestamp(),
        chat_model: opts.chat_model.clone(),
        judge_model: opts.judge_model.clone(),
        judge_trials: opts.judge_trials.max(1),
        max_tokens: opts.max_tokens,
        scenarios: reports,
        aggregate,
    })
}
