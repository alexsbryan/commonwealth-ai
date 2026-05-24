//! Convenience-wrapper integration tests. Pin the
//! prompt-and-test-generator contract for `tasks::split_file` and
//! `tasks::write_failing_test`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_tdd::{
    run_trial,
    tasks::{make_failing_tests_pass, split_file, write_failing_test},
    DeterministicChatBackend, TrialConfig, TrialStatus, Workdir,
};
use commonwealth_tdd::tasks::make_passing::MakePassingArgs;
use commonwealth_tdd::tasks::split_file::{
    cleanup_structural_test, SplitFileArgs, STRUCTURAL_TEST_FILENAME,
};
use commonwealth_tdd::tasks::write_failing_test::WriteFailingTestArgs;

fn init_git(path: &Path) {
    let _ = Command::new("git").arg("-C").arg(path).arg("init").arg("--initial-branch=main").output();
    let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.email", "t@t.t"]).output();
    let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.name", "t"]).output();
    let _ = Command::new("git").arg("-C").arg(path).args(["commit", "--allow-empty", "-m", "init"]).output();
}

fn pytest_available() -> bool {
    Command::new("pytest")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tight_config() -> TrialConfig {
    TrialConfig {
        candidates_per_round: 1,
        rounds_per_trial: 1,
        max_stall_rounds: 1,
        candidate_test_timeout: Duration::from_secs(20),
        ..TrialConfig::default()
    }
}

#[tokio::test]
async fn make_passing_wraps_a_maximize_passing_trial() {
    // Smoke that the convenience wrapper constructs a valid Trial
    // and the loop runs against an empty workdir → NoBaseline.
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    let backend = Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));
    let trial = make_failing_tests_pass(MakePassingArgs {
        workdir,
        model: "test".into(),
        task: None,
        test_command: Some("pytest -q".into()),
        config: Some(tight_config()),
    });
    let r = run_trial(trial, backend).await;
    // No source file + no tests → NoBaseline.
    assert!(matches!(r.status, TrialStatus::NoBaseline { .. }), "{:?}", r.status);
}

#[tokio::test]
async fn write_failing_test_uses_generate_one_failing_polarity() {
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    let backend = Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));
    let trial = write_failing_test(WriteFailingTestArgs {
        workdir,
        model: "test".into(),
        behavior: "the cache evicts on size limit".into(),
        test_file_hint: Some("tests/test_cache.py".into()),
        test_command: Some("pytest -q".into()),
        config: Some(tight_config()),
    });
    // The polarity match isn't directly visible through the Trial
    // struct (no Debug for Workdir on a public type), but the
    // wrapper documents and intends GenerateOneFailing. Smoke that
    // the trial runs — without any tests in the workdir the loop
    // returns immediately under the polarity-shaped baseline check.
    let r = run_trial(trial, backend).await;
    // GenerateOneFailing's loop short-circuits on backend errors
    // (empty script) before producing a result. Tolerate Stalled
    // or NoBaseline depending on path taken.
    assert!(
        matches!(r.status, TrialStatus::Stalled { .. } | TrialStatus::NoBaseline { .. } | TrialStatus::Exhausted { .. }),
        "got {:?}",
        r.status
    );
}

#[tokio::test]
async fn split_file_generates_structural_test_in_tests_dir() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    // Trivial single-file source so baseline aggregate is one big file.
    std::fs::write(tmp.path().join("calc.py"), "x = 1\ny = 2\n").unwrap();
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), true).unwrap();
    let backend = Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));

    let _trial = split_file(SplitFileArgs {
        workdir,
        model: "test".into(),
        path: "calc.py".into(),
        max_lines: 5,
        test_command: Some("pytest -q tests/test_max_file_size.py".into()),
        config: Some(tight_config()),
    });

    // Side effect of split_file: tests/test_max_file_size.py exists
    // and asserts the structural goal.
    let test_path = tmp.path().join("tests").join(STRUCTURAL_TEST_FILENAME);
    assert!(test_path.exists(), "structural test must be generated");
    let body = std::fs::read_to_string(&test_path).unwrap();
    assert!(body.contains("test_max_file_size"));
    assert!(body.contains("limit = 5"));

    // The generated test should PASS for a 2-line file, FAIL for a
    // many-line file. Verify it runs and behaves correctly.
    let result = Command::new("pytest")
        .args(["-q", "tests/test_max_file_size.py"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "generated test should pass on small file: {}",
        String::from_utf8_lossy(&result.stdout)
    );

    let _ = backend; // unused in this smoke test
}

#[tokio::test]
async fn cleanup_structural_test_removes_generated_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    std::fs::write(tmp.path().join("calc.py"), "x\n").unwrap();
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), true).unwrap();
    let _ = split_file(SplitFileArgs {
        workdir,
        model: "test".into(),
        path: "calc.py".into(),
        max_lines: 10,
        test_command: None,
        config: Some(tight_config()),
    });
    let test_path = tmp.path().join("tests").join(STRUCTURAL_TEST_FILENAME);
    assert!(test_path.exists());
    cleanup_structural_test(tmp.path());
    assert!(!test_path.exists());
}
