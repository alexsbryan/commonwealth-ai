//! Per-language test-result parsers. Dispatches over [`Language`].
//!
//! Lifted from `sovereign-agent-bench/src/witness/test_result_parser.rs`
//! preserving the validated parser shapes for the four languages the
//! solver loops drive (Rust libtest, Go test --json, vitest default,
//! pytest text). The tests in this module pin the empirical edge
//! cases (multi-binary cargo summaries, pytest -q quiet mode, etc).

use crate::shared::lang::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestParseResult {
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    pub failed_names: Vec<String>,
}

impl TestParseResult {
    pub fn pass_fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }
}

pub fn parse_test_output(language: Language, stdout: &str) -> TestParseResult {
    match language {
        Language::Rust => parse_cargo_libtest(stdout),
        Language::Go => parse_go_test_json(stdout),
        Language::TypeScript => parse_vitest_default(stdout),
        Language::Python => parse_pytest_text(stdout),
    }
}

pub fn parse_cargo_libtest(stdout: &str) -> TestParseResult {
    let mut failed_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("test ") {
            if let Some(name) = rest.strip_suffix(" ... FAILED") {
                failed_names.push(name.to_string());
            }
        }
    }
    let mut total_passed: u32 = 0;
    let mut total_failed: u32 = 0;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("test result:") {
            continue;
        }
        let (passed, failed) = parse_libtest_summary_line(trimmed);
        total_passed = total_passed.saturating_add(passed);
        total_failed = total_failed.saturating_add(failed);
    }
    let total = total_passed.saturating_add(total_failed);
    TestParseResult {
        passed: total_passed,
        failed: total_failed,
        total,
        failed_names,
    }
}

fn parse_libtest_summary_line(line: &str) -> (u32, u32) {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;
    for chunk in line.split(';') {
        let c = chunk.trim();
        if let Some(rest) = c.strip_suffix(" passed") {
            let n = rest.split_whitespace().last().unwrap_or("0");
            passed = n.parse().unwrap_or(0);
        } else if let Some(rest) = c.strip_suffix(" failed") {
            let n = rest.split_whitespace().last().unwrap_or("0");
            failed = n.parse().unwrap_or(0);
        }
    }
    (passed, failed)
}

pub fn parse_go_test_json(stdout: &str) -> TestParseResult {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;
    let mut failed_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let action = v.get("Action").and_then(|x| x.as_str()).unwrap_or("");
        let test = v.get("Test").and_then(|x| x.as_str()).unwrap_or("");
        if test.is_empty() {
            continue;
        }
        match action {
            "pass" => passed = passed.saturating_add(1),
            "fail" => {
                failed = failed.saturating_add(1);
                failed_names.push(test.to_string());
            }
            _ => {}
        }
    }
    let total = passed.saturating_add(failed);
    TestParseResult {
        passed,
        failed,
        total,
        failed_names,
    }
}

pub fn parse_vitest_default(stdout: &str) -> TestParseResult {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;
    let mut failed_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Tests") {
            passed = count_number_before_token(trimmed, "passed");
            failed = count_number_before_token(trimmed, "failed");
        }
        if let Some(name) = trimmed.strip_prefix("✗ ") {
            failed_names.push(name.to_string());
        } else if let Some(name) = trimmed.strip_prefix("× ") {
            failed_names.push(name.to_string());
        }
    }
    let total = passed.saturating_add(failed);
    TestParseResult {
        passed,
        failed,
        total,
        failed_names,
    }
}

fn count_number_before_token(s: &str, token: &str) -> u32 {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut found: Option<u32> = None;
    for i in 0..words.len().saturating_sub(1) {
        let next_clean = words[i + 1].trim_end_matches(|c: char| !c.is_ascii_alphabetic());
        if next_clean == token {
            let prev_clean = words[i].trim_start_matches(|c: char| !c.is_ascii_digit());
            if let Ok(n) = prev_clean.parse::<u32>() {
                found = Some(n);
            }
        }
    }
    found.unwrap_or(0)
}

pub fn parse_pytest_text(stdout: &str) -> TestParseResult {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;
    let mut failed_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("FAILED ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                failed_names.push(name);
            }
        }
        let stripped = trimmed.trim_matches('=').trim();
        if !stripped.contains("passed") && !stripped.contains("failed") {
            continue;
        }
        // Only a real count summary contains " in <duration>" — otherwise
        // a stray "FAILED foo - assert N passed" gets misread.
        let looks_like_summary = stripped.contains(" in ");
        if !looks_like_summary {
            continue;
        }
        let p = count_number_before_token(stripped, "passed");
        let f = count_number_before_token(stripped, "failed");
        if p > 0 || f > 0 {
            passed = p;
            failed = f;
        }
    }
    let total = passed.saturating_add(failed);
    TestParseResult {
        passed,
        failed,
        total,
        failed_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_libtest_all_pass() {
        let out = "\nrunning 4 tests\ntest a ... ok\ntest b ... ok\ntest c ... ok\ntest d ... ok\n\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 4);
        assert_eq!(r.failed, 0);
        assert_eq!(r.total, 4);
        assert!(r.failed_names.is_empty());
    }

    #[test]
    fn cargo_libtest_some_fail() {
        let out = "\nrunning 5 tests\ntest a ... ok\ntest b ... FAILED\ntest c ... ok\n\ntest result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 4);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["b".to_string()]);
    }

    #[test]
    fn cargo_libtest_handles_multiple_binaries() {
        let out = "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\ntest result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 5);
        assert_eq!(r.total, 5);
    }

    #[test]
    fn go_test_json_counts_test_events_only() {
        let out = r#"{"Action":"run","Test":"TestA"}
{"Action":"pass","Test":"TestA","Elapsed":0.01}
{"Action":"run","Test":"TestB"}
{"Action":"fail","Test":"TestB","Elapsed":0.02}
{"Action":"pass"}"#;
        let r = parse_go_test_json(out);
        assert_eq!(r.passed, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["TestB"]);
    }

    #[test]
    fn vitest_default_extracts_summary() {
        let out = "Tests  3 passed | 1 failed (4)\n✗ math.spec.ts > should add\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert!(r.failed_names.iter().any(|n| n.contains("should add")));
    }

    #[test]
    fn pytest_text_extracts_summary() {
        let out =
            "FAILED tests/test_a.py::test_b - assert 1 == 2\n=== 3 passed, 1 failed in 0.05s ===\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["tests/test_a.py::test_b"]);
    }

    #[test]
    fn pytest_text_quiet_mode_summary() {
        // `-q` mode: no `===` decoration. The empirical fix from
        // 3.2-lights-out-python 2026-05-22 — preserved here.
        let out =
            "......F.....\nFAILED tests/test_a.py::test_b - assert\n1 failed, 11 passed in 0.05s\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 11);
        assert_eq!(r.failed, 1);
    }

    #[test]
    fn pytest_text_doesnt_consume_failed_marker_lines() {
        let out = "FAILED tests/test_a.py::test_x\nFAILED tests/test_b.py::test_y\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 0);
        assert_eq!(r.failed_names.len(), 2);
    }
}
