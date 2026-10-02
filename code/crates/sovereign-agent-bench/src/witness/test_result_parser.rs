// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bench-side adapter over `sovereign_tdd`'s test-output parsers.
//!
//! The four per-language parsers (cargo libtest, `go test -json`,
//! vitest, pytest) and `TestParseResult` itself live in
//! `sovereign_tdd::shared::parser`. This module is the one arm
//! that maps the bench's [`WitnessLanguage`] onto tdd's `Language`.
//! The parsers used to be forked here; the fork had drifted — it
//! never grew tdd's 2026-07-07 "a run that broke before executing
//! tests is a FAILURE, not a run that never happened" fold, so a
//! candidate whose patch did not compile scored `total: 0` and was
//! indistinguishable from a harness fault.
//!
//! The tests below are retained deliberately. They stopped being a
//! second copy of tdd's coverage and became agent-bench's
//! consumer-contract suite over it: if tdd changes a parser shape
//! the bench depends on, these fail here.

use crate::problem::WitnessLanguage;

pub use sovereign_tdd::TestParseResult;

/// Dispatch over the witness language.
///
/// `WitnessLanguage` and `sovereign_tdd::Language` carry the same
/// four variants in the same order; this is a total map with no
/// fallback arm, so a new variant on either side is a compile error
/// rather than a silent mis-parse.
pub fn parse_test_output(language: WitnessLanguage, stdout: &str) -> TestParseResult {
    let lang = match language {
        WitnessLanguage::Rust => sovereign_tdd::Language::Rust,
        WitnessLanguage::Go => sovereign_tdd::Language::Go,
        WitnessLanguage::TypeScript => sovereign_tdd::Language::TypeScript,
        WitnessLanguage::Python => sovereign_tdd::Language::Python,
    };
    sovereign_tdd::shared::parser::parse_test_output(lang, stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_tdd::shared::parser::{
        parse_cargo_libtest, parse_go_test_json, parse_pytest_text, parse_vitest_default,
    };

    #[test]
    fn cargo_libtest_all_pass() {
        let out = "\nrunning 4 tests\ntest a ... ok\ntest b ... ok\ntest c ... ok\ntest d ... ok\n\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 4);
        assert_eq!(r.failed, 0);
        assert_eq!(r.total, 4);
        assert_eq!(r.pass_fraction(), 1.0);
        assert!(r.failed_names.is_empty());
    }

    #[test]
    fn cargo_libtest_some_fail() {
        let out = "\nrunning 5 tests\ntest a ... ok\ntest b ... FAILED\ntest c ... ok\ntest d ... ok\ntest e ... ok\n\nfailures:\n   b\n\ntest result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 4);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 5);
        assert!((r.pass_fraction() - 0.8).abs() < 1e-9);
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
        assert_eq!(r.total, 2);
        assert_eq!(r.failed_names, vec!["TestB"]);
    }

    #[test]
    fn vitest_default_extracts_summary() {
        let out = "Tests  3 passed | 1 failed (4)\n✗ math.spec.ts > should add\n";
        let r = parse_vitest_default(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 4);
        assert!(r.failed_names.iter().any(|n| n.contains("should add")));
    }

    #[test]
    fn pytest_text_extracts_summary() {
        let out =
            "FAILED tests/test_a.py::test_b - assert 1 == 2\n=== 3 passed, 1 failed in 0.05s ===\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 4);
        assert_eq!(r.failed_names, vec!["tests/test_a.py::test_b"]);
    }

    #[test]
    fn pytest_text_extracts_quiet_mode_summary() {
        // `-q` mode: no `===` decoration around the summary line.
        // Observed on 3.2-lights-out-python (2026-05-22) where the
        // parser previously read passed=0/total=0 despite stdout
        // showing "1 failed, 11 passed in 0.05s".
        let out =
            "......F.....\nFAILED tests/test_a.py::test_b - assert\n1 failed, 11 passed in 0.05s\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 11);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 12);
        assert_eq!(r.failed_names, vec!["tests/test_a.py::test_b"]);
    }

    #[test]
    fn pytest_text_all_pass_quiet() {
        let out = "............\n12 passed in 0.12s\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 12);
        assert_eq!(r.failed, 0);
        assert_eq!(r.total, 12);
    }

    #[test]
    fn pytest_text_doesnt_consume_failed_marker_lines() {
        // "FAILED tests/..." lines name failed tests but don't carry
        // counts. Without the "looks_like_summary" gate they could
        // be mistaken for a summary by the prior heuristic.
        let out = "FAILED tests/test_a.py::test_x\nFAILED tests/test_b.py::test_y\n";
        let r = parse_pytest_text(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 0);
        assert_eq!(r.failed_names.len(), 2);
    }

    /// A patch that does not COMPILE is a gradeable failure, not a
    /// witness that never ran. Cargo emits rustc diagnostics and no
    /// `test result:` summary; parsing that as `total: 0` collapses
    /// two distinct verdicts into one, and `scoring.rs` then persists
    /// `failed: 0, total: 0` — the established signal for "the
    /// witness never ran" — for a candidate that demonstrably ran and
    /// broke.
    #[test]
    fn cargo_compile_error_counts_as_failure_not_as_a_witness_that_never_ran() {
        let out = "   Compiling scratch v0.1.0\nerror[E0425]: cannot find function `is_palindrome` in this scope\n --> tests/new_behavior.rs:4:13\nerror: could not compile `scratch` (test \"new_behavior\") due to 1 previous error\n";
        let r = parse_cargo_libtest(out);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.total, 1);
        assert_eq!(r.failed_names, vec!["<compile error>".to_string()]);
    }

    #[test]
    fn pass_fraction_handles_zero_total() {
        let r = TestParseResult {
            passed: 0,
            failed: 0,
            total: 0,
            failed_names: vec![],
        };
        assert_eq!(r.pass_fraction(), 0.0);
    }
}
