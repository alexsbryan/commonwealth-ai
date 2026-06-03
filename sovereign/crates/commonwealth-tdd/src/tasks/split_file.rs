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

    // Materialize the structural goal as a test the loop's fitness
    // signal picks up. The template is framework-appropriate —
    // pytest's `test_max_file_size()`, cargo's `#[test] fn
    // max_file_size`, etc. — so the model's edits land in the same
    // language the workdir already uses.
    write_structural_test(workdir_path, framework, args.max_lines);

    let prompt = format!(
        "Goal: split `{}` until every source file is ≤ {} lines.\n\nThe generated test `max_file_size` (in the project's test directory) enforces this. Make it pass without breaking the others. Each turn, emit ONE EditAction — extract a function to a new file, inline a redundant helper, or rewrite the target file more compactly. The aggregate fitness is the test-pass count; you don't need to plan the whole refactor up front.",
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
    let (rel_path, content) = structural::render_max_file_size(framework, max_lines);
    let full = workdir.join(&rel_path);
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&full, content);
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
        assert!(body.contains("test_max_file_size"));
        assert!(body.contains("limit = 30"));
    }

    #[test]
    fn write_structural_test_creates_cargo_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::Cargo, 50);
        let p = tmp.path().join("tests/max_file_size.rs");
        let body = std::fs::read_to_string(&p).expect("test file written");
        assert!(body.contains("fn max_file_size"));
        assert!(body.contains("MAX_LINES: usize = 50"));
    }

    #[test]
    fn write_structural_test_creates_gotest_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_structural_test(tmp.path(), Framework::GoTest, 200);
        let p = tmp.path().join("max_file_size_test.go");
        let body = std::fs::read_to_string(&p).expect("test file written");
        assert!(body.contains("TestMaxFileSize"));
        assert!(body.contains("maxLines = 200"));
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
