// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end pipeline test — discovery → load → mock-agent run →
//! stub-judge → score → report → baseline persistence. No cargo
//! subprocess (the synthesized problem uses `JudgeOnly` for all
//! three dims so the auto-witness skips), no real pi.
//!
//! This proves the orchestration plumbing without depending on
//! external toolchains. The real witness pipeline is covered by
//! the unit tests under `witness::auto_test`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use sovereign_agent_bench::baseline::{read_latest, write_dated_and_update_latest};
use sovereign_agent_bench::cli::args::RunArgs;
use sovereign_agent_bench::cli::run::run_one_problem;
use sovereign_agent_bench::judge::{JudgeClient, JudgeError, JudgeRequest, JudgeTrialOutcome};
use sovereign_agent_bench::problem::load_problem;
use sovereign_agent_bench::report::BenchReport;
use sovereign_agent_bench::runner::{ExitReason, TokenCounts};
use sovereign_agent_bench::runners::mock::{MockAgentRunner, MockFileWrite, MockScript};

/// Deterministic judge for integration tests. Hands out anchor values
/// from a pre-seeded queue; rationale is the dimension name.
struct StubJudge {
    queue: Arc<Mutex<Vec<u8>>>,
}

impl StubJudge {
    fn new(anchors: Vec<u8>) -> Self {
        Self {
            queue: Arc::new(Mutex::new(anchors)),
        }
    }
}

#[async_trait]
impl JudgeClient for StubJudge {
    fn model_name(&self) -> &str {
        "stub"
    }

    async fn judge(&self, req: &JudgeRequest) -> Result<JudgeTrialOutcome, JudgeError> {
        let mut q = self.queue.lock().unwrap();
        let anchor = q.remove(0);
        Ok(JudgeTrialOutcome {
            raw: String::new(),
            anchor,
            rationale: format!(
                "stub-rationale-for-{}-anchor-{}",
                req.dimension_name, anchor
            ),
        })
    }
}

fn write_synthetic_problem(bench_root: &Path) {
    let problem_dir = bench_root.join("problems").join("99-test-synthetic");
    std::fs::create_dir_all(problem_dir.join("fixtures")).unwrap();
    std::fs::write(
        problem_dir.join("problem.toml"),
        r#"
[meta]
id = "99-test-synthetic"
title = "Synthetic test problem"
category = "Algorithmic"
version = "v0"

[prompt]
file = "prompt.md"

[witness]
kind = "JudgeOnly"
language = "Rust"
fixture_subdir = "fixtures"
verify_cmd = "true"
score_buckets = [[0.0, 1.001, 0]]

[budget]
token_cap = 1000
wall_seconds_cap = 30

[scoring.dim_a]
name = "Correctness"
mode = "JudgeRubric"
rubric_id = "dim_a"

[scoring.dim_b]
name = "Approach"
mode = "JudgeRubric"
rubric_id = "dim_b"

[scoring.dim_c]
name = "Efficiency"
mode = "JudgeRubric"
rubric_id = "dim_c"
"#,
    )
    .unwrap();
    std::fs::write(
        problem_dir.join("prompt.md"),
        "Solve the synthetic test problem.\n",
    )
    .unwrap();
    std::fs::write(
        problem_dir.join("rubric.md"),
        "## dim_a\n### 0\nwrong\n### 1\nok\n### 2\ngood\n### 3\noptimal\n\n## dim_b\n### 0\nwrong\n### 1\nok\n### 2\ngood\n### 3\noptimal\n\n## dim_c\n### 0\nwrong\n### 1\nok\n### 2\ngood\n### 3\noptimal\n",
    )
    .unwrap();
    // A non-empty fixtures dir is required by sandbox construction.
    std::fs::write(problem_dir.join("fixtures").join(".keep"), "").unwrap();
}

#[tokio::test]
async fn mock_runner_judge_only_pipeline_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let bench_root = tmp.path();
    write_synthetic_problem(bench_root);

    // Pre-seed three anchors per dimension (judge_trials=3) × 3 dims
    // = 9 anchor values.
    let stub = StubJudge::new(vec![3, 3, 3, 2, 2, 1, 3, 2, 3]);

    let mock = MockAgentRunner::new(MockScript {
        files: vec![MockFileWrite {
            path: "src/lib.rs".into(),
            contents: "pub fn solve() -> u32 { 7 }\n".into(),
        }],
        tool_calls: vec![],
        tokens: TokenCounts {
            input: 200,
            output: 80,
        },
        wall_ms: 1500,
        exit_reason: ExitReason::Completed,
        final_assistant_text: "Done.".into(),
    });

    let problem = load_problem(&bench_root.join("problems").join("99-test-synthetic")).unwrap();
    let args = RunArgs::parse(&[]).unwrap();

    let score = run_one_problem(&mock, &stub, &problem, &args, None)
        .await
        .expect("pipeline should complete");

    // 3,3,3 majority on dim_a → 3
    assert_eq!(score.dim_a.raw, 3, "dim_a anchors were [3,3,3]");
    // 2,2,1 majority on dim_b → 2
    assert_eq!(score.dim_b.raw, 2, "dim_b anchors were [2,2,1]");
    // 3,2,3 majority on dim_c → 3 (two 3s ≥ ⌈3/2⌉)
    assert_eq!(score.dim_c.raw, 3, "dim_c anchors were [3,2,3]");
    assert_eq!(score.total, 8);
    assert_eq!(score.tokens.output, 80);
    assert!(score.exit_reason.is_completed());
    assert!(!score.is_partial);

    // Persist + reload to confirm baseline plumbing.
    let report = BenchReport {
        agent: "mock".into(),
        model: "stub".into(),
        judge_model: "stub".into(),
        judge_trials: 3,
        run_trials: 1,
        started_at: "2026-05-20T12:00:00Z".into(),
        finished_at: "2026-05-20T12:01:00Z".into(),
        per_problem: vec![score.clone()],
        per_problem_trials: vec![],
        grand_total: score.total as u16,
        max_total: 9,
        regression: None,
    };
    let snapshot =
        write_dated_and_update_latest(bench_root, "agent-coding", "mock", "stub", &report)
            .expect("baseline write");
    assert!(snapshot.exists());
    let back: BenchReport = read_latest(bench_root, "agent-coding").unwrap().unwrap();
    assert_eq!(back.per_problem.len(), 1);
    assert_eq!(back.per_problem[0].problem_id, "99-test-synthetic");
    assert_eq!(back.per_problem[0].total, 8);
}

#[tokio::test]
async fn mock_runner_records_partial_when_tokens_exceeded() {
    let tmp = tempfile::tempdir().unwrap();
    let bench_root = tmp.path();
    write_synthetic_problem(bench_root);

    let stub = StubJudge::new(vec![1, 1, 1, 1, 1, 1, 1, 1, 1]);
    let mock = MockAgentRunner::new(MockScript {
        files: vec![],
        tool_calls: vec![],
        tokens: TokenCounts {
            input: 100,
            output: 1000,
        },
        wall_ms: 100,
        exit_reason: ExitReason::TokensExceeded {
            cap: 500,
            observed: 1000,
        },
        final_assistant_text: String::new(),
    });

    let problem = load_problem(&bench_root.join("problems").join("99-test-synthetic")).unwrap();
    let args = RunArgs::parse(&[]).unwrap();

    let score = run_one_problem(&mock, &stub, &problem, &args, None)
        .await
        .unwrap();
    assert!(score.is_partial);
    assert!(matches!(
        score.exit_reason,
        ExitReason::TokensExceeded { .. }
    ));
    // All anchors were 1 → total 3.
    assert_eq!(score.total, 3);
}
