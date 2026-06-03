//! End-to-end exercise of the unified `run_trial` against real
//! workdirs + real pytest + scripted backend. Same behavioral
//! surface as the pre-collapse green_loop / red_loop / refactor_loop
//! / multi_file_loop tests, consolidated under one function.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_tdd::{
    run_trial, DeterministicChatBackend, Polarity, Trial, TrialConfig, TrialStatus, Workdir,
};

fn init_git(path: &Path) {
    let _ = Command::new("git").arg("-C").arg(path).arg("init").arg("--initial-branch=main").output();
    let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.email", "t@t.t"]).output();
    let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.name", "t"]).output();
    let _ = Command::new("git").arg("-C").arg(path).args(["commit", "--allow-empty", "-m", "init"]).output();
}

fn write_committed(path: &Path, name: &str, content: &str) {
    let full = path.join(name);
    if let Some(p) = full.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    let _ = Command::new("git").arg("-C").arg(path).args(["add", name]).output();
    let _ = Command::new("git").arg("-C").arg(path).args(["commit", "-m", &format!("add {name}")]).output();
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

fn tight_config(candidates: usize, rounds: usize, stall: u32) -> TrialConfig {
    TrialConfig {
        candidates_per_round: candidates,
        rounds_per_trial: rounds,
        max_stall_rounds: stall,
        candidate_test_timeout: Duration::from_secs(20),
        ..TrialConfig::default()
    }
}

// ── MaximizePassing — short-circuit + improvement + stall ────────────

#[tokio::test]
async fn maximize_passing_short_circuits_when_baseline_already_green() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    write_committed(tmp.path(), "evaluator.py", "def add(a, b):\n    return a + b\n");
    write_committed(
        tmp.path(),
        "tests/test_evaluator.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nfrom evaluator import add\n\ndef test_add(): assert add(1, 2) == 3\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    let backend = Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "make tests pass".into(),
        test_command: "pytest -q tests/".into(),
        polarity: Polarity::MaximizePassing,
        config: tight_config(2, 3, 2),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(matches!(r.status, TrialStatus::Reached), "{:?}", r.status);
    assert_eq!(r.rounds, 0, "no rounds — short-circuit before backend");
    assert_eq!(backend.call_count(), 0);
}

#[tokio::test]
async fn maximize_passing_promotes_strict_improvement_to_reached() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    write_committed(tmp.path(), "evaluator.py", "def add(a, b):\n    return 0\n");
    write_committed(
        tmp.path(),
        "tests/test_evaluator.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nfrom evaluator import add\n\ndef test_add(): assert add(1, 2) == 3\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    // Script: round 0 → candidate 0 fixes it, candidate 1 is a no-op.
    let fix = r#"```json
{"action": "rewrite_function", "name": "add"}
```

```python
def add(a, b):
    return a + b
```"#;
    let noop = r#"```json
{"action": "rewrite_function", "name": "add"}
```

```python
def add(a, b):
    return 1
```"#;
    let backend = Arc::new(DeterministicChatBackend::from_strs(vec![
        fix.to_string(),
        noop.to_string(),
    ]));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "make failing tests pass".into(),
        test_command: "pytest -q tests/".into(),
        polarity: Polarity::MaximizePassing,
        config: tight_config(2, 3, 2),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(matches!(r.status, TrialStatus::Reached), "{:?}", r.status);
    assert_eq!(r.tests_after.passed, 1);
    assert_eq!(r.rounds, 1);
    assert!(r.trajectory[0].winner.is_some());
}

#[tokio::test]
async fn maximize_passing_stalls_when_no_candidate_improves() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    write_committed(tmp.path(), "evaluator.py", "def add(a, b):\n    return 0\n");
    write_committed(
        tmp.path(),
        "tests/test_evaluator.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nfrom evaluator import add\n\ndef test_add(): assert add(1, 2) == 3\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    let noop = r#"```json
{"action": "rewrite_function", "name": "add"}
```

```python
def add(a, b):
    return 1
```"#;
    let script = std::iter::repeat_n(noop.to_string(), 50).collect::<Vec<_>>();
    let backend = Arc::new(DeterministicChatBackend::from_strs(script));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "make tests pass".into(),
        test_command: "pytest -q tests/".into(),
        polarity: Polarity::MaximizePassing,
        config: tight_config(2, 6, 2),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(matches!(r.status, TrialStatus::Stalled { .. }), "{:?}", r.status);
    assert_eq!(r.tests_after.passed, 0);
}

#[tokio::test]
async fn maximize_passing_no_baseline_when_no_tests_in_workdir() {
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    let backend = Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "make tests pass".into(),
        test_command: "pytest -q".into(),
        polarity: Polarity::MaximizePassing,
        config: tight_config(1, 1, 1),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(matches!(r.status, TrialStatus::NoBaseline { .. }), "{:?}", r.status);
    assert_eq!(backend.call_count(), 0, "must not call backend without baseline");
}

// ── GenerateOneFailing — Red polarity ────────────────────────────────

#[tokio::test]
async fn generate_one_failing_accepts_test_that_fails_on_current_code() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    // Buggy source — `add` returns abs(a)+abs(b). The test the model
    // writes for "negative numbers preserve signs" will fail on this.
    write_committed(
        tmp.path(),
        "calculator.py",
        "def add(a, b):\n    return abs(a) + abs(b)\n",
    );
    // Need at least one baseline test so the loop has a starting
    // `total` to compare against.
    write_committed(
        tmp.path(),
        "tests/test_smoke.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nfrom calculator import add\n\ndef test_smoke(): assert add(1, 2) >= 0\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    // WriteFile now supports a `path` field — the model emits the
    // path where the new test file should land, the apply layer
    // routes there instead of clobbering the discovered source.
    let failing_test = r#"```json
{"action": "write_file", "path": "tests/test_negative.py"}
```

```python
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from calculator import add

def test_add_negative_numbers():
    assert add(-1, -2) == -3
"#;
    let backend = Arc::new(DeterministicChatBackend::from_strs(vec![
        failing_test.to_string(),
    ]));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "write a test for add() preserving signs on negatives".into(),
        test_command: "pytest -q tests/".into(),
        polarity: Polarity::GenerateOneFailing { test_name_hint: None },
        config: tight_config(1, 2, 1),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(
        matches!(r.status, TrialStatus::Reached),
        "expected Reached, got {:?} (trajectory: {:?})",
        r.status,
        r.trajectory
    );
    assert_eq!(r.tests_after.failed, 1);
    assert_eq!(r.tests_after.total, 2, "smoke + new test");
}

#[tokio::test]
async fn generate_one_failing_rejects_tautology() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    write_committed(tmp.path(), "calc.py", "def value(): return 42\n");
    write_committed(
        tmp.path(),
        "tests/test_calc.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nfrom calc import value\n\ndef test_value(): assert value() == 42\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
    // Candidate that doesn't change anything observable — rewrites
    // calc.py to the same logic. No failure added → no fitness
    // gain → stall.
    let noop = r#"```json
{"action": "write_file"}
```

```python
def value():
    return 42
"#;
    let script: Vec<String> = std::iter::repeat_n(noop.to_string(), 10).collect();
    let backend = Arc::new(DeterministicChatBackend::from_strs(script));
    let trial = Trial {
        workdir,
        model: "test".into(),
        prompt: "add a failing test".into(),
        test_command: "pytest -q tests/test_calc.py".into(),
        polarity: Polarity::GenerateOneFailing { test_name_hint: None },
        config: tight_config(1, 2, 1),
        syntax_validator: None,
    };
    let r = run_trial(trial, Arc::clone(&backend) as Arc<_>).await;
    assert!(
        matches!(r.status, TrialStatus::Stalled { .. }),
        "expected Stalled, got {:?}",
        r.status
    );
    assert_eq!(r.tests_after.failed, 0, "no new failure was introduced");
}
