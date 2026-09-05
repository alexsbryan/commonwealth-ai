// SPDX-License-Identifier: AGPL-3.0-or-later
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
    // Demand MACHINE-READABLE output from whatever the verify command runs.
    //
    // Everything downstream of here PARSES this output — how many tests
    // passed, how many failed, which ones. `FORCE_COLOR` (set by several agent
    // harnesses and CI runners) makes pytest and CPython emit ANSI escapes even
    // into a pipe, and then `1 error in 0.06s` arrives as
    // `\x1b[31m\x1b[1m1 error\x1b[0m\x1b[31m in 0.06s\x1b[0m` and matches
    // nothing. The run does not fail — it reports `0p/0f`, which reads as "no
    // tests" rather than "could not read the result", so the solver stalls
    // against a repo that is working fine (ARCH §18.3: absence reported as a
    // result). Observed 2026-08-25 under `FORCE_COLOR=3`.
    //
    // This is the one seam every verify command goes through, whatever the
    // language, so the normalisation belongs here and not in each parser.
    command
        .env("NO_COLOR", "1")
        .env("PYTHON_COLORS", "0")
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE");
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
            // A timeout returns an EMPTY result (0p/0f) — indistinguishable
            // downstream from "the tests ran and found nothing". Warn so a
            // killed-under-load run is diagnosable rather than silently
            // reading as "no progress" and stalling the solve loop
            // (2026-07-16). If this fires in a unit test, the candidate
            // timeout is too tight for the load the suite runs under.
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                "run_tests: test command timed out; returning empty (0p/0f)"
            );
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
    let mut tail_text = tail(&combined, 1500);
    // Playwright (1.49+) writes an aria snapshot of the page next to
    // each failure — the give-the-model-eyes move, in text. Appended
    // to the tail so the next round's prompt can read what the page
    // actually showed. No-op for every non-Playwright run: the
    // test-results directory doesn't exist.
    if let Some(aria) = newest_error_context(workdir) {
        tail_text.push_str("\n---page state at failure (aria snapshot)---\n");
        tail_text.push_str(&head(&aria, 1500));
    }
    TestRunResult {
        parsed,
        tail: tail_text,
    }
}

/// Newest `error-context.md` under `<workdir>/test-results/`.
/// Playwright clears that directory at the start of each run, so
/// whatever is there belongs to the run that just finished.
fn newest_error_context(workdir: &Path) -> Option<String> {
    let root = workdir.join("test-results");
    if !root.is_dir() {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let mut stack = vec![(root, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 3 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push((p, depth + 1));
            } else if p.file_name().is_some_and(|n| n == "error-context.md") {
                if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                    if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                        best = Some((mtime, p));
                    }
                }
            }
        }
    }
    let text = std::fs::read_to_string(best?.1).ok()?;
    Some(strip_instructions_section(&text))
}

/// Drop the file's `# Instructions` section — it tells a reader to
/// "explain why" and "provide a snippet", which contradicts the
/// solver's emission format. The model gets the facts (test info,
/// error details, page snapshot), not competing instructions.
fn strip_instructions_section(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    for line in text.lines() {
        if line.starts_with("# ") {
            skipping = line == "# Instructions";
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// First `max_bytes` of `s`, cut on a char boundary.
fn head(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... (truncated)", &s[..end])
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
