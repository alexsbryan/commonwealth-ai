// SPDX-License-Identifier: AGPL-3.0-or-later
//! Situated-lane judging loop.
//!
//! No generation happens here — see [`super::transcripts`] for why. The loop
//! is: for each banked probe, bind the criteria its question type calls for,
//! judge each one against the response the production turn produced, and
//! aggregate through the shared rubric core.

use std::time::Instant;

use sovereign_inference::remote::RemoteApiProvider;

use super::criteria::{self, Vocabulary};
use super::report::{ProbeReport, SituatedRun};
use super::transcripts::Transcript;
use crate::bench_cmd::rubric::judge::{self, CriterionVerdict};
use crate::bench_cmd::rubric::score::{aggregate, score_item, CriterionOutcome};

pub struct RunOptions {
    pub daemon_base: String,
    pub judge_model: String,
    pub judge_trials: u8,
    pub subject_model: String,
    pub transcripts_path: String,
    pub criteria_path: String,
    pub transcripts_skipped: usize,
}

pub async fn run(
    transcripts: &[Transcript],
    vocab: &Vocabulary,
    opts: &RunOptions,
) -> Result<SituatedRun, String> {
    let v1 = format!("{}/v1", opts.daemon_base.trim_end_matches('/'));
    let judge_provider = RemoteApiProvider::new(&v1, None, &opts.judge_model, 16384);

    if opts.subject_model == opts.judge_model {
        eprintln!(
            "bench situated: WARNING — judge model equals the model under test ({}); \
             self-judging conflates the rater with the rated (ARCH_PRINCIPLES §7.6). \
             Pin a different --judge-model for comparable scores.",
            opts.judge_model
        );
    }

    let mut probes: Vec<ProbeReport> = Vec::with_capacity(transcripts.len());
    for (idx, t) in transcripts.iter().enumerate() {
        let bound = criteria::bind(vocab, &t.id, t.qtype);
        eprintln!(
            "bench situated: [{}/{}] {} ({}, {} criteria)",
            idx + 1,
            transcripts.len(),
            t.id,
            t.qtype.label(),
            bound.len()
        );

        let started = Instant::now();
        let judged_text = t.judged_text();
        let mut outcomes = Vec::with_capacity(bound.len());
        for c in &bound {
            // STRUCTURAL criteria are READ from what the production gate
            // decided, never re-derived by the judge. `cites_a_source` is the
            // whole reason this branch exists: the gate resolves the
            // chunk→section join and knows exactly how many locators it
            // released, while a judge can only squint at the prose. Measured
            // 2026-08-05 on the saltgrass compound bank, the judged version
            // scored 3 real citations as 1 — it credited the one answer that
            // did NOT disclose a gap and declined both that did, because a
            // trailing "the passages do not answer X" distracted it, and it
            // stayed wrong under --judge-trials 3, so this was a stable bias
            // and not noise. The answer it rewarded was also the fabricating
            // one. A criterion that pays for fabrication and fines disclosure
            // cannot measure situatedness, and no rewording fixes a decider
            // that is downstream of the fact (§10.6).
            //
            // Only criteria whose truth the gate actually OWNS belong here.
            // Everything else stays judged — "is the gap statement specific",
            // "is this padded", "does it lecture" are genuine judgements the
            // system holds no answer to.
            if c.key == "cites_a_source" {
                let verdict = match t.cites_a_source() {
                    Some(cited) => CriterionVerdict {
                        verdict: Some(if cited {
                            judge::Judgement::Yes
                        } else {
                            judge::Judgement::No
                        }),
                        evidence: format!(
                            "structural: gate released {} located quote(s)",
                            t.citation_located.unwrap_or(0)
                        ),
                        // One "trial" — the gate's own decision. Recorded as a
                        // trial so the per-criterion table reads uniformly, and
                        // it is deterministic, so --judge-trials cannot move it.
                        trials_yes: u32::from(cited),
                        trials_no: u32::from(!cited),
                        trials_failed: 0,
                    },
                    // ONLY a transcript banked before the gate persisted the
                    // count lands here — a turn that abstained or took the
                    // legacy ladder writes 0, which is a miss, not an unknown.
                    // Scored as could-not-judge so an old transcript cannot
                    // manufacture a regression it never had (§18.3).
                    None => CriterionVerdict {
                        verdict: None,
                        evidence: "structural: transcript predates the gate's located-quote count"
                            .to_string(),
                        trials_yes: 0,
                        trials_no: 0,
                        trials_failed: 0,
                    },
                };
                outcomes.push(CriterionOutcome {
                    criterion_id: c.id.clone(),
                    dimension: c.dimension.clone(),
                    weight: c.weight,
                    verdict,
                });
                eprint!("=");
                continue;
            }
            // An empty response is could-not-judge on every criterion, not a
            // zero: nothing was observed, so nothing may be scored.
            let verdict = if t.is_judgeable() {
                judge::judge_criterion(
                    &judge_provider,
                    &judged_text,
                    &c.text,
                    Some(&opts.judge_model),
                    opts.judge_trials,
                )
                .await
            } else {
                CriterionVerdict {
                    verdict: None,
                    evidence: "no visible response to judge".to_string(),
                    trials_yes: 0,
                    trials_no: 0,
                    trials_failed: 0,
                }
            };
            outcomes.push(CriterionOutcome {
                criterion_id: c.id.clone(),
                dimension: c.dimension.clone(),
                weight: c.weight,
                verdict,
            });
            eprint!(".");
        }
        eprintln!();
        let judge_ms_total = started.elapsed().as_millis() as u64;

        let could_not_judge = outcomes
            .iter()
            .filter(|o| o.verdict.verdict.is_none())
            .count();
        let score = score_item(&outcomes);
        match score {
            Some(s) => eprintln!(
                "  score {s:.1}  (judge {:.1}s)",
                judge_ms_total as f64 / 1000.0
            ),
            None => eprintln!("  score n/a — every criterion could-not-judge"),
        }
        probes.push(ProbeReport {
            probe_id: t.id.clone(),
            qtype: t.qtype.label().to_string(),
            score,
            criteria: outcomes,
            could_not_judge,
            response: t.answer.clone(),
            gate_action: t.gate_action.clone(),
            judge_ms_total,
        });
    }

    let aggregate = aggregate(&probes);
    Ok(SituatedRun {
        started_at_unix: chrono::Utc::now().timestamp(),
        subject_model: opts.subject_model.clone(),
        judge_model: opts.judge_model.clone(),
        judge_trials: opts.judge_trials.max(1),
        transcripts_path: opts.transcripts_path.clone(),
        criteria_path: opts.criteria_path.clone(),
        criteria_version: vocab.meta.version,
        criteria_status: vocab.meta.status.clone(),
        transcripts_skipped: opts.transcripts_skipped,
        probes,
        aggregate,
    })
}
