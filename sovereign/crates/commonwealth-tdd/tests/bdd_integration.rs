//! End-to-end exercise of the bdd_cycle composition with a
//! scripted backend + real pytest. Pins the contract that
//! synthesis Reached → green runs → tests stay green or improve.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_tdd::tasks::bdd::{bdd_cycle, BddCycleArgs, ReviewMode};
use commonwealth_tdd::{DeterministicChatBackend, TrialConfig, TrialStatus, Workdir};

fn init_git(path: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("init")
        .arg("--initial-branch=main")
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "user.email", "t@t.t"])
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "user.name", "t"])
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["commit", "--allow-empty", "-m", "init"])
        .output();
}

fn write_committed(path: &Path, name: &str, content: &str) {
    let full = path.join(name);
    if let Some(p) = full.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["add", name])
        .output();
    let _ = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["commit", "-m", &format!("add {name}")])
        .output();
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
        rounds_per_trial: 2,
        max_stall_rounds: 1,
        candidate_test_timeout: Duration::from_secs(20),
        ..TrialConfig::default()
    }
}

#[tokio::test]
async fn auto_mode_runs_synthesis_then_green() {
    if !pytest_available() {
        eprintln!("pytest not on PATH — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_git(tmp.path());
    // Existing source with a stub function; baseline has 1 smoke
    // test that passes.
    write_committed(
        tmp.path(),
        "calculator.py",
        "def add(a, b):\n    raise NotImplementedError\n",
    );
    write_committed(
        tmp.path(),
        "tests/test_smoke.py",
        "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\nimport calculator  # noqa\n\ndef test_smoke(): assert True\n",
    );
    let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();

    // Stage 1 (synthesis) script: write a failing test for add().
    // WriteFile with explicit path so it lands at tests/test_add.py.
    let synthesis = r#"```json
{"action": "write_file", "path": "tests/test_add.py"}
```

```python
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from calculator import add

def test_add_positive():
    assert add(2, 3) == 5
"#;
    // Stage 2 (green) script: rewrite the function to make the
    // failing test pass.
    let green = r#"```json
{"action": "rewrite_function", "name": "add"}
```

```python
def add(a, b):
    return a + b
```"#;
    let backend: Arc<dyn commonwealth_tdd::ChatBackend> =
        Arc::new(DeterministicChatBackend::from_strs(vec![
            synthesis.to_string(),
            green.to_string(),
        ]));
    let r = bdd_cycle(
        BddCycleArgs {
            workdir,
            model: "test".into(),
            intent: "add(a, b) returns the sum of its arguments".into(),
            test_file_hint: Some("tests/test_add.py".into()),
            task_hint: Some("implement the failing add() test".into()),
            test_command: Some("pytest -q tests/".into()),
            config: Some(tight_config()),
            review_mode: ReviewMode::Auto,
        },
        backend,
    )
    .await;

    // Synthesis must have reached the failing-test state.
    assert!(
        matches!(r.synthesis.status, TrialStatus::Reached),
        "synthesis must Reach, got {:?}",
        r.synthesis.status
    );
    // Generated test path + content surfaced.
    assert!(r.generated_test_path.is_some());
    assert!(r.generated_test_content.is_some());
    let content = r.generated_test_content.as_ref().unwrap();
    assert!(content.contains("test_add_positive"));

    // Green stage must have run and reached.
    let green = r.green.expect("Auto must run green when synthesis Reached");
    assert!(
        matches!(green.status, TrialStatus::Reached),
        "green must Reach after synthesis succeeded, got {:?}",
        green.status
    );
    // Final state: smoke + the new test both pass.
    assert!(green.tests_after.passed >= 2);
}
