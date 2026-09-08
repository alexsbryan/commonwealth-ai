// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-language test-result parsers. Dispatches over [`Language`].
//!
//! Lifted from `sovereign-agent-bench/src/witness/test_result_parser.rs`
//! preserving the validated parser shapes for the four languages the
//! solver loops drive (Rust libtest, Go test --json, vitest default,
//! pytest text). The tests in this module pin the empirical edge
//! cases (multi-binary cargo summaries, pytest -q quiet mode, etc).
//!
//! ## Setup errors count as failures (2026-07-07)
//!
//! When the framework demonstrably RAN and broke before executing
//! tests — pytest collection error, cargo compile error, vitest/jest
//! suite error, go build failure — the parser folds the breakage
//! into `failed`/`total` with a marked name (`<setup error: …>`,
//! `<compile error>`, …). Without this fold the most idiomatic TDD
//! opening — a pin test that imports a function that doesn't exist
//! yet — parses as 0 tests and is invisible to BOTH polarities'
//! fitness (live SOLVE receipts, job 419d4d3f): the Red stage can't
//! accept it and the Green stage sees `NoBaseline` instead of a
//! gradient. A test command that never ran (binary missing, usage
//! error) still parses 0/0/0 — that distinction keeps `NoBaseline`
//! meaning "there is nothing here to steer by".

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
    let mut saw_summary = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("test result:") {
            continue;
        }
        saw_summary = true;
        let (passed, failed) = parse_libtest_summary_line(trimmed);
        total_passed = total_passed.saturating_add(passed);
        total_failed = total_failed.saturating_add(failed);
    }
    // Compile failure: rustc diagnostics and no test summary at all.
    // A test that references a not-yet-written function dies here —
    // count it as one failing entry so the loop has a gradient. When
    // any summary exists, tests ran; the "error: test failed" footer
    // cargo prints after real failures must not double-count.
    if !saw_summary {
        let compile_error = stdout.lines().any(|l| {
            let t = l.trim();
            t.starts_with("error[") || t.starts_with("error:")
        });
        if compile_error {
            failed_names.push("<compile error>".to_string());
            total_failed = 1;
        }
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
    let mut package_failures: u32 = 0;
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
            // Package-level fail with no Test field = build failure
            // (or a package that broke before its tests ran).
            if action == "fail" {
                package_failures = package_failures.saturating_add(1);
            }
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
    // Packages failed but no test ever ran → build failure. One
    // failing entry gives the loop a gradient instead of a void.
    if passed == 0 && failed == 0 && package_failures > 0 {
        failed = 1;
        failed_names.push("<build failed>".to_string());
    }
    let total = passed.saturating_add(failed);
    TestParseResult {
        passed,
        failed,
        total,
        failed_names,
    }
}

/// Parses the TypeScript-ecosystem reporters: vitest default, jest
/// default, AND Playwright's line reporter. One parser because the
/// dispatch key is [`Language`] (from the source file's extension),
/// which can't tell a vitest project from a Playwright one — the
/// output shapes are disjoint enough to coexist.
pub fn parse_vitest_default(stdout: &str) -> TestParseResult {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;
    let mut errors: u32 = 0;
    let mut suite_failures: u32 = 0;
    let mut playwright_fatal = false;
    let mut failed_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        // Suite-level failures ("Test Files  1 failed (1)" — vitest;
        // "Test Suites: 1 failed, 1 total" — jest). A syntax or
        // import error fails the whole file before any test runs.
        if trimmed.starts_with("Test Files") || trimmed.starts_with("Test Suites") {
            suite_failures = count_number_before_token(trimmed, "failed");
        } else if trimmed.starts_with("Tests") {
            passed = count_number_before_token(trimmed, "passed");
            failed = count_number_before_token(trimmed, "failed");
        } else if let Some((n, kind)) = playwright_summary(trimmed) {
            // Playwright line-reporter summary: bare `3 passed (4.2s)`
            // / `1 failed` / `2 errors` lines.
            match kind {
                "passed" => passed = n,
                "failed" => failed = n,
                "errors" => errors = n,
                _ => {}
            }
        }
        // Playwright numbered failure header:
        //   `1) [chromium] › tests/x.spec.ts:9:5 › title ─────`
        if let Some(name) = playwright_failure_name(trimmed) {
            failed_names.push(name);
        }
        if let Some(name) = trimmed.strip_prefix("✗ ") {
            failed_names.push(name.to_string());
        } else if let Some(name) = trimmed.strip_prefix("× ") {
            failed_names.push(name.to_string());
        } else if let Some(name) = trimmed.strip_prefix("✘ ") {
            failed_names.push(name.to_string());
        }
        // Playwright fatals that abort before any test: a dead
        // webServer, or an Error in a run that demonstrably was
        // Playwright ("›" separators / spec paths present overall).
        if trimmed.contains("Process from config.webServer") {
            playwright_fatal = true;
        }
    }
    // "Error" bare on purpose: fatals arrive as `Error:`, but also
    // `SyntaxError [Error]:` (broken config) and friends. Safe at
    // this width because the branch below only fires when ZERO
    // counts parsed and the output is demonstrably a Playwright run.
    if !playwright_fatal
        && stdout.contains("Error")
        && !stdout.contains("No tests found")
        && (stdout.contains(".spec.ts")
            || stdout.contains("[chromium]")
            || stdout.contains("playwright"))
        && passed == 0
        && failed == 0
        && errors == 0
    {
        playwright_fatal = true;
    }
    failed_names.dedup();
    // Ran-and-broke fold (same rule as pytest/cargo): suites or the
    // whole run broke before tests executed → failing entries, so
    // the loop has a gradient. "No tests found" stays 0/0/0 — that
    // is the NoBaseline signal the fix→pin fallthrough steers by.
    if passed == 0 && failed == 0 {
        if suite_failures > 0 {
            failed = suite_failures;
            failed_names.push("<suite error>".to_string());
        } else if errors > 0 {
            failed = errors;
            failed_names.push("<suite error>".to_string());
        } else if playwright_fatal {
            failed = 1;
            failed_names.push("<suite error>".to_string());
        }
    } else {
        // Tests ran; global errors still count alongside them.
        failed = failed.saturating_add(errors);
        if errors > 0 {
            failed_names.push("<suite error>".to_string());
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

/// `"3 passed (4.2s)"` → `Some((3, "passed"))`. The Playwright line
/// reporter's summary shape: a bare count, an outcome word, and
/// nothing after except an optional `(duration)`. The trailing check
/// keeps prose like "3 passed the review" from registering.
fn playwright_summary(line: &str) -> Option<(u32, &'static str)> {
    let mut words = line.split_whitespace();
    let n: u32 = words.next()?.parse().ok()?;
    let kind = match words.next()? {
        "passed" => "passed",
        "failed" => "failed",
        "error" | "errors" => "errors",
        "flaky" | "skipped" | "interrupted" => "ignored",
        _ => return None,
    };
    match words.next() {
        None => Some((n, kind)),
        Some(next) if next.starts_with('(') => Some((n, kind)),
        Some(_) => None,
    }
}

/// Playwright failure listing names, both shapes:
///   `✘ [chromium] › tests/x.spec.ts:9:5 › title` (handled by ✘ strip)
///   `1) [chromium] › tests/x.spec.ts:9:5 › title ─────`
fn playwright_failure_name(line: &str) -> Option<String> {
    let (digits, rest) = line.split_once(") ")?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !rest.contains(" › ") {
        return None;
    }
    Some(rest.trim_end_matches(['─', ' ']).to_string())
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
    let mut errors: u32 = 0;
    let mut failed_names: Vec<String> = Vec::new();
    let mut error_names: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("FAILED ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                failed_names.push(name);
            }
        }
        // Collection/setup error markers: `ERROR tests/test_x.py`.
        // The space (not colon) distinguishes them from usage errors
        // like "ERROR: file or directory not found" — those mean the
        // run never collected anything and must stay invisible.
        if let Some(rest) = trimmed.strip_prefix("ERROR ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                error_names.push(format!("<setup error: {name}>"));
            }
        }
        let stripped = trimmed.trim_matches('=').trim();
        if !stripped.contains("passed")
            && !stripped.contains("failed")
            && !stripped.contains("error")
        {
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
        let e = count_number_before_token(stripped, "error")
            .max(count_number_before_token(stripped, "errors"));
        if p > 0 || f > 0 || e > 0 {
            passed = p;
            failed = f;
            errors = e;
        }
    }
    // Fold collection/setup errors into the failure counts so a pin
    // test that fails at import time (function doesn't exist yet)
    // registers as a failing test instead of vanishing.
    if errors > 0 {
        if error_names.is_empty() {
            error_names.push("<setup error>".to_string());
        }
        error_names.truncate(errors as usize);
        failed_names.extend(error_names);
        failed = failed.saturating_add(errors);
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

    // ── setup-errors-count-as-failures (2026-07-07) ────────────────

    #[test]
    fn pytest_collection_error_counts_as_failure() {
        // The idiomatic pin: test imports a function that doesn't
        // exist yet → collection error, zero tests collected.
        let out = "==== ERRORS ====\nERROR tests/test_new_behavior.py\nImportError: cannot import name 'is_palindrome' from 'utils'\n=== 1 error in 0.05s ===\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 1);
        assert_eq!(
            r.failed_names,
            vec!["<setup error: tests/test_new_behavior.py>"]
        );
    }

    #[test]
    fn pytest_mixed_passes_and_collection_error() {
        let out = "ERROR tests/test_new.py\n=== 3 passed, 1 error in 0.20s ===\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 4);
    }

    #[test]
    fn pytest_no_tests_ran_stays_invisible() {
        // Genuinely-no-tests must stay 0/0/0 — it is the NoBaseline
        // signal solve's fix→pin fallthrough steers by.
        let out = "no tests ran in 0.03s\n";
        let r = parse_pytest_text(out);
        assert_eq!((r.passed, r.failed, r.total), (0, 0, 0));
    }

    #[test]
    fn pytest_usage_error_stays_invisible() {
        // `ERROR:` (colon) is pytest's usage-error prefix — the run
        // never collected anything; must not synthesize a failure.
        let out = "ERROR: file or directory not found: tests/\n";
        let r = parse_pytest_text(out);
        assert_eq!((r.passed, r.failed, r.total), (0, 0, 0));
        assert!(r.failed_names.is_empty());
    }

    #[test]
    fn cargo_compile_error_counts_as_failure() {
        let out = "   Compiling scratch v0.1.0\nerror[E0425]: cannot find function `is_palindrome` in this scope\n --> tests/new_behavior.rs:4:13\nerror: could not compile `scratch` (test \"new_behavior\") due to 1 previous error\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 1);
        assert_eq!(r.failed_names, vec!["<compile error>"]);
    }

    #[test]
    fn cargo_real_failures_do_not_double_count_the_error_footer() {
        // After genuine test failures cargo prints "error: test
        // failed, to rerun ..." — a summary exists, so no synthesis.
        let out = "test a ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored\nerror: test failed, to rerun pass `--lib`\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 2);
    }

    #[test]
    fn cargo_command_not_found_stays_invisible() {
        let out = "sh: cargo: command not found\n";
        let r = parse_cargo_libtest(out);
        assert_eq!((r.passed, r.failed, r.total), (0, 0, 0));
    }

    #[test]
    fn vitest_suite_error_with_no_tests_counts_as_failure() {
        let out = "Test Files  1 failed (1)\nTests  no tests\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["<suite error>"]);
    }

    #[test]
    fn vitest_suite_failures_do_not_inflate_real_test_counts() {
        let out = "Test Files  1 failed (2)\nTests  3 passed | 1 failed (4)\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 4);
    }

    // ── playwright line reporter (SOLVE_PLAYWRIGHT) ────────────────

    #[test]
    fn playwright_line_summary_counts() {
        let out = "Running 4 tests using 1 worker\n\n  1) [chromium] › tests/save.spec.ts:9:5 › save shows toast ─────\n\n    Error: expect(locator).toBeVisible() failed\n\n  1 failed\n    [chromium] › tests/save.spec.ts:9:5 › save shows toast\n  3 passed (4.2s)\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 4);
        assert!(
            r.failed_names
                .iter()
                .any(|n| n.contains("save.spec.ts:9:5 › save shows toast")),
            "{:?}",
            r.failed_names
        );
    }

    #[test]
    fn playwright_all_passing() {
        let out = "Running 3 tests using 1 worker\n\n  3 passed (2.1s)\n";
        let r = parse_vitest_default(out);
        assert_eq!((r.passed, r.failed, r.total), (3, 0, 3));
    }

    #[test]
    fn playwright_webserver_death_counts_as_suite_error() {
        let out = "Error: Process from config.webServer was not able to start. Exit code: 1\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["<suite error>"]);
    }

    #[test]
    fn playwright_no_tests_found_stays_invisible() {
        // NoBaseline signal — the fix→pin fallthrough depends on it.
        let out = "Error: No tests found\n";
        let r = parse_vitest_default(out);
        assert_eq!((r.passed, r.failed, r.total), (0, 0, 0));
    }

    #[test]
    fn playwright_spec_load_error_counts_as_suite_error() {
        let out = "Error: tests/pin.spec.ts: Unexpected token (3:7)\n\n  1 error\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["<suite error>"]);
    }

    #[test]
    fn playwright_broken_config_counts_as_suite_error() {
        // Real shape from a live run (job 09777dfe): babel reports
        // `SyntaxError [Error]:` — no bare `Error:` substring, no
        // summary counts at all.
        let out = "SyntaxError [Error]: /app/playwright.config.ts: Missing semicolon. (14:6)\n    at constructor (/app/node_modules/playwright/lib/transform/babelBundle.js:14617:23)\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["<suite error>"]);
    }

    #[test]
    fn playwright_prose_numbers_do_not_register() {
        let out = "✓ 1 [chromium] › tests/x.spec.ts:3:1 › 3 passed the review checks\n";
        let r = parse_vitest_default(out);
        assert_eq!((r.passed, r.failed, r.total), (0, 0, 0));
    }

    #[test]
    fn go_build_failure_counts_as_one_failure() {
        let out = r##"{"Action":"start","Package":"scratch"}
{"Action":"output","Package":"scratch","Output":"# scratch\n"}
{"Action":"fail","Package":"scratch","Elapsed":0}"##;
        let r = parse_go_test_json(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["<build failed>"]);
    }

    #[test]
    fn go_package_fail_after_real_test_failures_not_double_counted() {
        let out = r#"{"Action":"fail","Test":"TestB","Elapsed":0.02}
{"Action":"fail","Package":"scratch","Elapsed":0.1}"#;
        let r = parse_go_test_json(out);
        assert_eq!(r.failed, 1);
        assert_eq!(r.failed_names, vec!["TestB"]);
    }
}
