//! Run a shell command in a workdir with timeout + signal handling,
//! returning parsed test results.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use crate::shared::lang::Language;
use crate::shared::parser::{parse_test_output, TestParseResult};

#[derive(Debug, Clone)]
pub struct TestRunResult {
    pub parsed: TestParseResult,
    /// Last ~1.5 KB of combined stdout/stderr — feeds the next
    /// prompt round so the model can read the last failure.
    pub tail: String,
}

impl TestRunResult {
    pub fn empty(reason: &str) -> Self {
        Self {
            parsed: TestParseResult {
                passed: 0,
                failed: 0,
                total: 0,
                failed_names: vec![],
            },
            tail: reason.to_string(),
        }
    }
}

pub async fn run_tests(
    workdir: &Path,
    verify_cmd: &str,
    language: Language,
    timeout: Duration,
) -> TestRunResult {
    if verify_cmd.trim().is_empty() {
        return TestRunResult::empty("verify_cmd is empty");
    }
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(verify_cmd)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return TestRunResult::empty(&format!("spawn failed: {e}")),
    };
    let pid = child.id();
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return TestRunResult::empty(&format!("wait failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            if let Some(p) = pid {
                // The child may have spawned its own children (pytest
                // → python; cargo test → test binary). Kill the whole
                // process group so orphaned children don't outlive
                // the candidate's timeout.
                let pgid = format!("-{p}");
                let _ = std::process::Command::new("kill")
                    .args(["-KILL", "--", &pgid])
                    .status();
            }
            return TestRunResult::empty(&format!("timeout after {}s", timeout.as_secs()));
        }
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        combined.push_str("\n---stderr---\n");
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let parsed = parse_test_output(language, &combined);
    TestRunResult {
        parsed,
        tail: tail(&combined, 1500),
    }
}

fn tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = s.len() - max_bytes;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("... (truncated)\n{}", &s[start..])
}
