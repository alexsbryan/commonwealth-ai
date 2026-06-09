// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-witness driver. Copies held-out fixtures into the agent's
//! workdir, runs `verify_cmd`, parses the output, scores a pass
//! fraction.
//!
//! The fixture copy is *after* the agent exits — fixtures live
//! outside the workdir during the run per ARCH §7.2.
//!
//! Helper `run_verify_cmd` is duplicated (not imported) from
//! `sovereign-tools::code::atos_utils` per ARCH §8.5 (heavy deps
//! stay in the crate that needs them) + §10.3 (helpers over trait
//! gymnastics for small duplication: a 25-line helper duplicated
//! once is below the smell threshold).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tracing::{info, warn};

use crate::problem::Problem;
use crate::witness::test_result_parser::{parse_test_output, TestParseResult};

#[derive(Debug, Clone)]
pub struct AutoWitnessOutcome {
    pub verify_exit_ok: bool,
    pub stdout_tail: String,
    pub parsed: TestParseResult,
    pub bucketed_score: u8, // 0..=3
}

#[derive(Debug, Error)]
pub enum AutoWitnessError {
    #[error("fixture source missing: {0}")]
    FixtureSourceMissing(PathBuf),
    #[error("fixture copy failed: {0}")]
    FixtureCopy(String),
    #[error("verify command failed to spawn: {0}")]
    VerifySpawn(String),
}

/// Run the auto-witness against `workdir`. Returns the bucketed
/// 0..=3 score for the Correctness dimension.
pub async fn run_auto_witness(
    problem: &Problem,
    workdir: &Path,
) -> Result<AutoWitnessOutcome, AutoWitnessError> {
    // 1. Copy held-out fixtures into the workdir, OVERWRITING any
    //    same-named files the agent may have produced. Hidden fixtures
    //    win.
    let fixture_src = problem.fixture_path();
    if !fixture_src.is_dir() {
        return Err(AutoWitnessError::FixtureSourceMissing(fixture_src));
    }
    copy_dir_recursive(&fixture_src, workdir)
        .map_err(|e| AutoWitnessError::FixtureCopy(e.to_string()))?;

    // 2. Run verify_cmd from inside the workdir. Cap wall time at
    // 2× the agent's wall budget — a model that writes an
    // infinite-loop test would otherwise hang the witness
    // indefinitely (observed 2026-05-21: 1.2 trial 1 stuck for 20+
    // min in `cargo test` because model's two_sum impl had a
    // non-terminating loop). 2× is generous: the agent's own wall
    // budget already accommodates cold-compile + iteration; the
    // witness only needs a fresh compile + the test run, so 2×
    // covers cold-target rebuild even on a slow disk.
    // Tight per-call cap on the witness verify. Same fix as the
    // executor's run_shell: if the model wrote an infinite loop,
    // pytest can spin for 30+ min at 100% CPU eating the bench's
    // total wall (observed 4.2 2026-05-23). The witness's verify
    // is one shot of build+test; 180s is generous for well-formed
    // code while bounding the pathological case sharply.
    let witness_wall = std::time::Duration::from_secs(180);
    let (verify_exit_ok, stdout_tail) =
        run_verify_cmd(workdir, &problem.witness.verify_cmd, witness_wall).await;

    // 3. Parse + bucket.
    let parsed = parse_test_output(problem.witness.language, &stdout_tail);
    let bucketed = bucket_pass_fraction(parsed.pass_fraction(), &problem.witness.score_buckets);
    info!(
        problem = %problem.meta.id,
        language = problem.witness.language.id(),
        verify_exit_ok,
        passed = parsed.passed,
        failed = parsed.failed,
        total = parsed.total,
        pass_fraction = parsed.pass_fraction(),
        bucketed,
        "agent_bench: witness"
    );
    Ok(AutoWitnessOutcome {
        verify_exit_ok,
        stdout_tail,
        parsed,
        bucketed_score: bucketed,
    })
}

/// Bucket a pass fraction into 0..=3 against the per-problem
/// `score_buckets` `[low_inclusive, high_exclusive, score]`. When the
/// buckets list is empty (or no bucket matches), a sensible default
/// quartile bucketing applies.
pub fn bucket_pass_fraction(frac: f64, buckets: &[[f64; 3]]) -> u8 {
    if buckets.is_empty() {
        return default_quartile(frac);
    }
    for row in buckets {
        let low = row[0];
        let high = row[1];
        let score = row[2] as u8;
        if frac >= low && frac < high {
            return score.min(3);
        }
    }
    // Above the last bucket's `high` → take the top bucket's score.
    if let Some(last) = buckets.last() {
        return (last[2] as u8).min(3);
    }
    default_quartile(frac)
}

fn default_quartile(frac: f64) -> u8 {
    if frac < 0.25 {
        0
    } else if frac < 0.6 {
        1
    } else if frac < 0.85 {
        2
    } else {
        3
    }
}

/// Recursive copy of `src` into `dst`. Existing files at the target
/// path are overwritten (the held-out fixtures take precedence over
/// anything the agent wrote with the same name).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            // Overwrite if it exists.
            std::fs::copy(entry.path(), &target)?;
        }
        // Symlinks are intentionally skipped — fixtures shouldn't have any.
    }
    Ok(())
}

/// Run a shell verify command in `workdir`. Returns `(exit_ok, stdout_tail)`.
/// Duplicated from `sovereign-tools::code::atos_utils::run_verify_cmd`
/// per ARCH §8.5 — taking a dep on sovereign-tools just for this 25-line
/// helper would pull lancedb + treesitter + arrow into a leaf crate.
async fn run_verify_cmd(
    workdir: &Path,
    cmd: &str,
    wall_cap: std::time::Duration,
) -> (bool, String) {
    if cmd.trim().is_empty() {
        return (false, "verify_cmd is empty".into());
    }
    // Spawn the subprocess directly (not via .output()) so we can
    // SIGKILL it if `tokio::time::timeout` fires. Without this kill,
    // an infinite-loop test (e.g. agent's broken two_sum running a
    // non-terminating while loop) would leave a zombie cargo
    // process consuming a core indefinitely.
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Same process-group trick as executor::run_shell so kill on
    // timeout reaches grandchildren (sh's child pytest) instead of
    // leaking them as init-owned orphans.
    #[cfg(unix)]
    command.process_group(0);
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "agent_bench: verify spawn failed");
            return (false, format!("verify spawn failed: {e}"));
        }
    };
    // Capture PID before moving child into wait_with_output, so the
    // timeout path can kill the whole process group.
    let child_pid = child.id();
    let wait_future = child.wait_with_output();
    let output = match tokio::time::timeout(wall_cap, wait_future).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warn!(error = %e, "agent_bench: verify wait failed");
            return (false, format!("verify wait failed: {e}"));
        }
        Err(_) => {
            // Kill the full process group, not just sh — sh's
            // grandchildren (e.g. pytest) would otherwise leak.
            #[cfg(unix)]
            if let Some(pid) = child_pid {
                let pgid_arg = format!("-{pid}");
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &pgid_arg])
                    .status();
            }
            warn!(
                wall_cap_secs = wall_cap.as_secs(),
                "agent_bench: verify wall-cap fired — verify subprocess killed"
            );
            return (
                false,
                format!(
                    "verify exceeded wall cap of {}s — killed",
                    wall_cap.as_secs()
                ),
            );
        }
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        combined.push_str("\n---stderr---\n");
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    // Keep the TAIL not the head: every test framework (cargo, pytest,
    // go test, vitest) prints its pass/fail summary as the LAST line.
    // The witness's `parse_test_output` reads that summary; if we keep
    // the head, the summary is lost when verbose failure tracebacks
    // exceed the cap. Observed 2026-05-23 on 4.2-mini-evaluator (20
    // failing tests with full tracebacks → 462 lines → 16K head cut
    // off the count line → witness reported passed=0/total=0).
    //
    // 32K is sufficient for the typical 20-test failing run (the
    // failure block per test is ~600 bytes including diff) while
    // still bounding context. Together with tail-keeping this gives
    // both the summary AND the most-recent ~50 test failures.
    let truncated = truncate_tail(&combined, 32 * 1024);
    (output.status.success(), truncated)
}

/// Keep the LAST `limit` bytes of `s`, UTF-8 safe. When truncated,
/// prefix with a marker so readers know prefix was dropped.
fn truncate_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    // Walk forward from `s.len() - limit` to the next char boundary
    // so we never slice mid-codepoint.
    let mut cut = s.len() - limit;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    format!("... (truncated head: {} bytes dropped)\n{}", cut, &s[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::problem::WitnessLanguage;

    #[test]
    fn truncate_tail_keeps_end_not_start() {
        // Regression: 2026-05-23 witness reported passed=0/total=0
        // on 4.2 because head-truncation cut the pytest summary
        // line ("20 failed in 0.05s") at the end of the output.
        // Tail-keeping ensures the summary survives.
        let body: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let tail_marker = "20 failed in 0.05s\n";
        let combined = format!("{body}{tail_marker}");
        let cut = truncate_tail(&combined, 200);
        assert!(
            cut.contains("20 failed in 0.05s"),
            "summary line lost: {cut}"
        );
        assert!(cut.contains("(truncated head:"));
    }

    #[test]
    fn truncate_tail_safe_for_short_inputs() {
        assert_eq!(truncate_tail("short", 100), "short");
    }

    #[test]
    fn truncate_tail_utf8_safe() {
        // Multibyte char straddling the truncation point — must
        // walk forward to next char boundary instead of slicing
        // mid-codepoint.
        let s = format!("{}—tail", "x".repeat(100));
        let cut = truncate_tail(&s, 50);
        assert!(cut.contains("tail"));
        assert!(cut.starts_with("... (truncated head:"));
    }

    #[test]
    fn bucket_pass_fraction_default_quartile() {
        assert_eq!(default_quartile(0.0), 0);
        assert_eq!(default_quartile(0.24), 0);
        assert_eq!(default_quartile(0.25), 1);
        assert_eq!(default_quartile(0.59), 1);
        assert_eq!(default_quartile(0.6), 2);
        assert_eq!(default_quartile(0.84), 2);
        assert_eq!(default_quartile(0.85), 3);
        assert_eq!(default_quartile(1.0), 3);
    }

    #[test]
    fn bucket_pass_fraction_uses_per_problem_buckets() {
        let buckets = vec![
            [0.0, 0.25, 0.0],
            [0.25, 0.6, 1.0],
            [0.6, 0.85, 2.0],
            [0.85, 1.001, 3.0],
        ];
        assert_eq!(bucket_pass_fraction(0.1, &buckets), 0);
        assert_eq!(bucket_pass_fraction(0.5, &buckets), 1);
        assert_eq!(bucket_pass_fraction(0.8, &buckets), 2);
        assert_eq!(bucket_pass_fraction(1.0, &buckets), 3);
    }

    #[test]
    fn bucket_pass_fraction_above_top_takes_top_score() {
        // Edge case: 1.0 might fall outside if buckets end at 1.0.
        let buckets = vec![[0.0, 0.5, 0.0], [0.5, 1.0, 3.0]];
        assert_eq!(bucket_pass_fraction(1.0, &buckets), 3);
    }

    #[test]
    fn copy_dir_recursive_creates_target_and_overwrites() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("tests")).unwrap();
        std::fs::write(src.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(src.path().join("tests/integration.rs"), "// real\n").unwrap();
        // Pre-existing file in dst that should be overwritten.
        std::fs::write(dst.path().join("Cargo.toml"), "OVERWRITE ME").unwrap();
        copy_dir_recursive(src.path(), dst.path()).unwrap();
        let ct = std::fs::read_to_string(dst.path().join("Cargo.toml")).unwrap();
        assert!(ct.contains("[package]"));
        assert!(dst.path().join("tests/integration.rs").exists());
    }

    fn test_wall() -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }

    #[tokio::test]
    async fn run_verify_cmd_exit_zero_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let (ok, out) = run_verify_cmd(tmp.path(), "true", test_wall()).await;
        assert!(ok);
        assert!(out.is_empty() || !out.contains("verify spawn failed"));
    }

    #[tokio::test]
    async fn run_verify_cmd_exit_nonzero_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let (ok, _) = run_verify_cmd(tmp.path(), "exit 7", test_wall()).await;
        assert!(!ok);
    }

    #[tokio::test]
    async fn run_verify_cmd_empty_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let (ok, msg) = run_verify_cmd(tmp.path(), "", test_wall()).await;
        assert!(!ok);
        assert!(msg.contains("empty"));
    }

    #[tokio::test]
    async fn run_verify_cmd_wall_cap_fires_on_infinite_loop() {
        // Class E regression: an infinite-loop verify subprocess
        // must be killed and reported as failed instead of hanging
        // forever. Without the wall cap, the sweep would block
        // indefinitely on the first model that writes a
        // non-terminating test (observed 2026-05-21).
        let tmp = tempfile::tempdir().unwrap();
        let (ok, msg) = run_verify_cmd(
            tmp.path(),
            "while true; do :; done",
            std::time::Duration::from_secs(1),
        )
        .await;
        assert!(!ok);
        assert!(
            msg.contains("wall cap") || msg.contains("killed"),
            "expected wall-cap message, got: {msg}"
        );
    }

    #[test]
    fn parse_test_output_dispatches_per_language() {
        let rust_out =
            "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_test_output(WitnessLanguage::Rust, rust_out);
        assert_eq!(r.passed, 4);
    }
}
