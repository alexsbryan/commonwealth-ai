//! N-trial judge aggregator. Mirrors the shape from
//! `sovereign-cli/src/eval_cmd/runner_threads.rs:597-680` but built
//! against the 0..=3 anchor index instead of per-fact coverage.
//!
//! Aggregation:
//!   - majority_anchor: the anchor that appeared in `>= ⌈N/2⌉` trials,
//!     ties broken by the lower index (matches the eval bank's
//!     deterministic-tiebreak convention).
//!   - coverage_mean: `mean(anchor_i / 3.0)` over the trials — a
//!     continuous-valued signal that captures how decisively the
//!     trials agree (1.0 = unanimous top anchor; 0.0 = unanimous
//!     bottom).
//!
//! Trials run sequentially to keep the daemon load predictable. We
//! could parallelise but the daemon is single-slot today and
//! parallel calls just queue server-side.

use serde::{Deserialize, Serialize};

use crate::judge::{JudgeClient, JudgeError, JudgeRequest, JudgeTrialOutcome};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiTrialOutcome {
    pub trials: Vec<JudgeTrialOutcome>,
    pub majority_anchor: u8,
    /// `mean(anchor_i / 3.0)` — continuous signal across trials.
    pub coverage_mean: f64,
    /// Whether the majority was reached with at least ⌈N/2⌉ votes.
    pub majority_reached: bool,
}

pub async fn run_judge_trials<C: JudgeClient + ?Sized>(
    client: &C,
    req: &JudgeRequest,
    trials: u8,
) -> Result<MultiTrialOutcome, JudgeError> {
    let trials = trials.max(1);
    let mut outcomes: Vec<JudgeTrialOutcome> = Vec::with_capacity(trials as usize);
    for i in 0..trials {
        tracing::debug!(
            problem = %req.problem_id,
            dim = %req.dimension_name,
            trial = i + 1,
            total = trials,
            "agent_bench: judge trial"
        );
        let trial = client.judge(req).await?;
        outcomes.push(trial);
    }
    Ok(aggregate(outcomes, trials))
}

pub fn aggregate(trials: Vec<JudgeTrialOutcome>, total: u8) -> MultiTrialOutcome {
    let mut counts: [u32; 4] = [0; 4];
    let mut sum_f: f64 = 0.0;
    for t in &trials {
        if (t.anchor as usize) < 4 {
            counts[t.anchor as usize] += 1;
        }
        sum_f += (t.anchor as f64) / 3.0;
    }
    let denom = trials.len().max(1) as f64;
    let coverage_mean = sum_f / denom;

    let needed = (total as u32).div_ceil(2); // ⌈N/2⌉
    let majority_anchor = pick_majority(&counts, needed).unwrap_or_else(|| {
        // Fallback: highest-count anchor with low-index tiebreak.
        let mut best: (u32, u8) = (0, 0);
        for (i, c) in counts.iter().enumerate() {
            if *c > best.0 {
                best = (*c, i as u8);
            }
        }
        best.1
    });
    let majority_reached = counts[majority_anchor as usize] >= needed;
    MultiTrialOutcome {
        trials,
        majority_anchor,
        coverage_mean,
        majority_reached,
    }
}

fn pick_majority(counts: &[u32; 4], needed: u32) -> Option<u8> {
    // Lower index wins ties.
    for (i, c) in counts.iter().enumerate() {
        if *c >= needed {
            return Some(i as u8);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct StubClient {
        anchors: Mutex<Vec<u8>>,
    }
    impl StubClient {
        fn new(anchors: Vec<u8>) -> Self {
            Self {
                anchors: Mutex::new(anchors),
            }
        }
    }
    #[async_trait]
    impl JudgeClient for StubClient {
        async fn judge(&self, _req: &JudgeRequest) -> Result<JudgeTrialOutcome, JudgeError> {
            let mut guard = self.anchors.lock().unwrap();
            let next = guard.remove(0);
            Ok(JudgeTrialOutcome {
                anchor: next,
                rationale: format!("trial-anchor-{next}"),
            })
        }
    }

    fn fake_req() -> JudgeRequest {
        JudgeRequest {
            problem_id: "1.1".into(),
            problem_prompt: "p".into(),
            dimension_name: "d".into(),
            rubric_anchors: ["a0".into(), "a1".into(), "a2".into(), "a3".into()],
            workspace_view: "ws".into(),
            final_assistant_text: "t".into(),
        }
    }

    #[tokio::test]
    async fn three_trials_majority_two_two_one() {
        let client = StubClient::new(vec![2, 2, 1]);
        let out = run_judge_trials(&client, &fake_req(), 3).await.unwrap();
        assert_eq!(out.majority_anchor, 2);
        assert!(out.majority_reached);
        // (2 + 2 + 1) / 3 / 3 = 5/9 ≈ 0.5555...
        assert!((out.coverage_mean - 5.0 / 9.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn three_trials_no_majority_falls_back_to_plurality() {
        // 1, 2, 3 — each has 1 vote; no anchor meets ⌈3/2⌉ = 2.
        let client = StubClient::new(vec![1, 2, 3]);
        let out = run_judge_trials(&client, &fake_req(), 3).await.unwrap();
        // Fallback picks the highest-count anchor; on a tie, the lower
        // index wins (matches eval-bank convention).
        assert_eq!(out.majority_anchor, 1);
        assert!(!out.majority_reached);
    }

    #[tokio::test]
    async fn unanimous_top_anchor_gives_full_coverage_mean() {
        let client = StubClient::new(vec![3, 3, 3]);
        let out = run_judge_trials(&client, &fake_req(), 3).await.unwrap();
        assert_eq!(out.majority_anchor, 3);
        assert!((out.coverage_mean - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn single_trial_supported() {
        let client = StubClient::new(vec![2]);
        let out = run_judge_trials(&client, &fake_req(), 1).await.unwrap();
        assert_eq!(out.majority_anchor, 2);
        assert!(out.majority_reached);
    }
}
