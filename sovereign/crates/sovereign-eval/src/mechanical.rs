//! Mechanical scoring — run the frozen golden test crate against the
//! agent's working tree.
//!
//! The golden crate at `<experiment-repo>/scorer/golden/` is its own
//! workspace and path-deps `oicp-types = { path = "../.." }`. We invoke
//! `cargo test --manifest-path .../scorer/golden/Cargo.toml --no-fail-fast`
//! and parse the JSON test events stream.
//!
//! Per the ARCH_PRINCIPLES of this project: never run `cargo test`
//! against the agent's main workspace via Bash (watcher contention).
//! The golden crate is a SEPARATE workspace, so this is safe — it
//! doesn't touch the daemon's watched corpus.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MechanicalReport {
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_ignored: u32,
    pub tests_total: u32,
    pub failed_test_names: Vec<String>,
    pub compile_failed: bool,
    pub compile_error_excerpt: Option<String>,
    pub raw_stdout_truncated: String,
    pub raw_stderr_truncated: String,
}

const RAW_LIMIT: usize = 16_384;

/// Run the golden suite and return a parsed report.
///
/// `golden_manifest`: path to `scorer/golden/Cargo.toml` (the
/// freestanding test workspace).
pub fn run(golden_manifest: &Path) -> Result<MechanicalReport> {
    if !golden_manifest.exists() {
        bail!("golden manifest not found at {}", golden_manifest.display());
    }

    let out = Command::new("cargo")
        .arg("test")
        .arg("--manifest-path")
        .arg(golden_manifest)
        .arg("--no-fail-fast")
        .arg("--")
        .arg("--format=json")
        .arg("-Z")
        .arg("unstable-options")
        .env("RUSTC_BOOTSTRAP", "1")
        .output()
        .context("spawning cargo test")?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let compile_failed = stderr.contains("error[E") || stderr.contains("error: could not compile");
    let compile_error_excerpt = if compile_failed {
        Some(extract_compile_excerpt(&stderr))
    } else {
        None
    };

    if compile_failed {
        return Ok(MechanicalReport {
            tests_passed: 0,
            tests_failed: 0,
            tests_ignored: 0,
            tests_total: 0,
            failed_test_names: vec![],
            compile_failed: true,
            compile_error_excerpt,
            raw_stdout_truncated: truncate(&stdout, RAW_LIMIT),
            raw_stderr_truncated: truncate(&stderr, RAW_LIMIT),
        });
    }

    let mut report = parse_json_events(&stdout);
    report.raw_stdout_truncated = truncate(&stdout, RAW_LIMIT);
    report.raw_stderr_truncated = truncate(&stderr, RAW_LIMIT);
    Ok(report)
}

fn parse_json_events(stdout: &str) -> MechanicalReport {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut ignored = 0u32;
    let mut failed_names: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') || !line.ends_with('}') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
        if ty != "test" {
            continue;
        }
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        match event {
            "ok" => passed += 1,
            "failed" => {
                failed += 1;
                failed_names.push(name);
            }
            "ignored" => ignored += 1,
            _ => {}
        }
    }

    let total = passed + failed + ignored;
    MechanicalReport {
        tests_passed: passed,
        tests_failed: failed,
        tests_ignored: ignored,
        tests_total: total,
        failed_test_names: failed_names,
        compile_failed: false,
        compile_error_excerpt: None,
        raw_stdout_truncated: String::new(),
        raw_stderr_truncated: String::new(),
    }
}

fn extract_compile_excerpt(stderr: &str) -> String {
    let mut lines: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            l.contains("error[E")
                || l.starts_with("error:")
                || l.starts_with("  --> ")
                || l.contains("expected")
                || l.starts_with("note:")
        })
        .take(60)
        .collect();
    if lines.is_empty() {
        lines = stderr.lines().take(40).collect();
    }
    lines.join("\n")
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        let mut head = s[..limit / 2].to_string();
        head.push_str("\n... (truncated) ...\n");
        head.push_str(&s[s.len() - limit / 2..]);
        head
    }
}

/// Discover the golden manifest path under an experiment repo.
/// Returns `<experiment_repo>/scorer/golden/Cargo.toml` if it exists.
pub fn discover_golden_manifest(experiment_repo: &Path) -> Option<PathBuf> {
    let p = experiment_repo.join("scorer").join("golden").join("Cargo.toml");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_events_counts_outcomes() {
        let stdout = r#"
{"type":"suite","event":"started","test_count":3}
{"type":"test","event":"started","name":"a"}
{"type":"test","event":"ok","name":"a"}
{"type":"test","event":"started","name":"b"}
{"type":"test","event":"failed","name":"b","stdout":"oh no"}
{"type":"test","event":"ignored","name":"c"}
{"type":"suite","event":"ok","passed":1,"failed":1,"ignored":1,"measured":0,"filtered_out":0}
"#;
        let r = parse_json_events(stdout);
        assert_eq!(r.tests_passed, 1);
        assert_eq!(r.tests_failed, 1);
        assert_eq!(r.tests_ignored, 1);
        assert_eq!(r.tests_total, 3);
        assert_eq!(r.failed_test_names, vec!["b".to_string()]);
    }

    #[test]
    fn truncate_handles_short_input() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_handles_long_input() {
        let s: String = std::iter::repeat('x').take(200).collect();
        let t = truncate(&s, 50);
        assert!(t.len() < 200);
        assert!(t.contains("(truncated)"));
    }
}
