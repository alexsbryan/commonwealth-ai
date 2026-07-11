// SPDX-License-Identifier: AGPL-3.0-or-later
//! Split-file task — translates a structural goal ("every source
//! file ≤ N lines") into a runtime test + a Trial.
//!
//! Language-agnostic as of 2026-05-24: dispatches via
//! `tasks::framework::detect_framework` and
//! `tasks::structural::max_file_size::render` so the generated
//! test is in the project's actual test framework (pytest / cargo /
//! vitest / jest / go-test).

use std::path::{Path, PathBuf};

use crate::tasks::framework::detect_framework;
use crate::tasks::structural;
use crate::types::{Polarity, Trial, TrialConfig};
use crate::workdir::Workdir;

pub struct SplitFileArgs {
    pub workdir: Workdir,
    pub model: String,
    /// File the user is focused on. Surfaces in the prompt so the
    /// model knows where to look first. The structural test
    /// considers ALL source files, not just this one.
    pub path: PathBuf,
    pub max_lines: usize,
    pub test_command: Option<String>,
    pub config: Option<TrialConfig>,
}

pub fn split_file(args: SplitFileArgs) -> Trial {
    let workdir_path = args.workdir.path();
    let framework = detect_framework(workdir_path);
    let test_command = args
        .test_command
        .unwrap_or_else(|| framework.default_test_command().to_string());

    // Materialize the structural goal as a LADDER of tests the
    // loop's fitness signal can climb. A single threshold makes a
    // refactor a cliff (every extraction that shrinks the largest
    // file but misses the final budget ties and is discarded —
    // agent-bench 3.3 receipts, 2026-07-07); rungs from the current
    // worst file size down to the target make each extraction a
    // strict improvement.
    write_structural_test(workdir_path, framework, args.max_lines);

    let prompt = format!(
        "Goal: split `{}` until every source file is ≤ {} lines.\n\nThe generated `max_file_size` test ladder (in the project's test directory) enforces this at descending thresholds — each extraction that shrinks the largest file flips another rung. Make them pass without breaking the behavior tests. Extract cohesive helpers to new files (emit multiple action+block pairs in one response when the step needs coordinated changes); you don't need to plan the whole refactor up front.",
        args.path.display(),
        args.max_lines,
    );

    Trial {
        workdir: args.workdir,
        model: args.model,
        prompt,
        test_command,
        polarity: Polarity::MaximizePassing,
        config: args.config.unwrap_or_default(),
        syntax_validator: None,
    }
}

/// Write the framework-appropriate structural test into the
/// workdir. Creates the parent directory if needed; overwrites
/// any prior generated file.
pub fn write_structural_test(
    workdir: &Path,
    framework: crate::tasks::framework::Framework,
    max_lines: usize,
) {
    let worst = worst_source_file_lines(workdir, framework);
    let rungs = structural::ladder_rungs(max_lines, worst);
    let (rel_path, content) = structural::render_max_file_size_ladder(framework, &rungs);
    let full = workdir.join(&rel_path);
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&full, content);
}

/// Largest source-file line count in the workdir for the framework's
/// language — seeds the ladder's top rung. Skips test/build dirs.
fn worst_source_file_lines(workdir: &Path, framework: crate::tasks::framework::Framework) -> usize {
    use crate::tasks::framework::Framework;
    let exts: &[&str] = match framework {
        Framework::Pytest => &["py"],
        Framework::Cargo => &["rs"],
        Framework::Vitest | Framework::Jest | Framework::Playwright => &["ts", "tsx", "js", "jsx"],
        Framework::GoTest => &["go"],
    };
    fn walk(dir: &Path, exts: &[&str], worst: &mut usize) {
        const SKIP: &[&str] = &[
            "target",
            "node_modules",
            ".git",
            "tests",
            "build",
            "dist",
            "__pycache__",
        ];
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if SKIP.iter().any(|x| *x == s) || s.starts_with('.') {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                walk(&p, exts, worst);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
            {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    *worst = (*worst).max(text.lines().count());
                }
            }
        }
    }
    let mut worst = 0;
    walk(workdir, exts, &mut worst);
    worst
}

/// Remove the framework-appropriate structural test from the
/// workdir. No-op if the file doesn't exist.
pub fn cleanup_structural_test(workdir: &Path, framework: crate::tasks::framework::Framework) {
    let (rel_path, _) = structural::render_max_file_size(framework, 0);
    let _ = std::fs::remove_file(workdir.join(rel_path));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::framework::Framework;

    #[test]
    fn write_structural_test_creates_pytest_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::Pytest, 30);
        let p = tmp.path().join("tests/test_max_file_size.py");
        let body = std::fs::read_to_string(&p).expect("test file written");
        assert!(body.contains("test_max_file_size_within_30"));
        assert!(body.contains("def _over"));
    }

    #[test]
    fn write_structural_test_creates_cargo_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::Cargo, 50);
        let p = tmp.path().join("tests/max_file_size.rs");
        let body = std::fs::read_to_string(&p).expect("test file written");
        assert!(body.contains("fn max_file_size_within_50"));
        assert!(body.contains("fn over(max_lines: usize)"));
    }

    #[test]
    fn write_structural_test_creates_gotest_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::GoTest, 200);
        let p = tmp.path().join("max_file_size_test.go");
        let body = std::fs::read_to_string(&p).expect("test file written");
        assert!(body.contains("TestMaxFileSizeWithin200"));
    }

    #[test]
    fn cleanup_removes_framework_specific_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::Pytest, 30);
        let p = tmp.path().join("tests/test_max_file_size.py");
        assert!(p.exists());
        cleanup_structural_test(tmp.path(), Framework::Pytest);
        assert!(!p.exists());
    }
}
