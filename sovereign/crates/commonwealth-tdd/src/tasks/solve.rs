// SPDX-License-Identifier: AGPL-3.0-or-later
//! The verbless coding-goal entry point — `solve(workdir, goal)`.
//!
//! This is the composition the SOLVE surface (daemon jobs, MCP,
//! CLI) dispatches to. It makes the goal test-shaped and drives it
//! green, choosing the path from what the workdir already has:
//!
//! - **Failing tests present** → one `MaximizePassing` trial with
//!   the goal as the task prompt (the `fix` path).
//! - **No tests, or everything already green** → the goal isn't
//!   test-shaped yet: pin it with one synthesized failing test,
//!   then drive that green (the `bdd_cycle` composition).
//!
//! The probe is free: the fix trial's own baseline run answers
//! "are there failing tests?" — `NoBaseline` and zero-round
//! `Reached` are the two fall-through signals, so no extra test
//! run happens on the fix path.
//!
//! An explicit [`SolveVerb`] skips inference for the rare goal
//! where the default isn't what you meant.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backend::ChatBackend;
use crate::tasks::bdd::{bdd_cycle_observed, BddCycleArgs, BddRoundObserver, BddStage, ReviewMode};
use crate::tasks::make_passing::{make_failing_tests_pass, MakePassingArgs};
use crate::tasks::split_file::{split_file, SplitFileArgs};
use crate::tasks::write_failing_test::{write_failing_test, WriteFailingTestArgs};
use crate::trial::run_trial_observed;
use crate::types::{RoundObserver, RoundSummary, TrialConfig, TrialResult, TrialStatus};
use crate::workdir::Workdir;

/// Explicit path override. `None` on [`SolveArgs::verb`] means
/// "infer from the workdir" — the default and the common case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolveVerb {
    /// Drive currently-failing tests green.
    Fix,
    /// Write the one failing test that pins the goal; don't fix it.
    Pin,
    /// Split oversized files via the structural test ladder.
    Split { max_lines: usize },
}

pub struct SolveArgs {
    pub workdir: Workdir,
    pub model: String,
    /// Plain-language coding goal. Threaded verbatim into whichever
    /// trial prompt the dispatch picks.
    pub goal: String,
    pub verb: Option<SolveVerb>,
    /// Override the auto-detected test command.
    pub test_command: Option<String>,
    pub config: Option<TrialConfig>,
}

/// Which stage of the composition a streamed round belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveStage {
    /// MaximizePassing against existing failing tests.
    Fix,
    /// Red — synthesizing the goal-pinning failing test.
    Pin,
    /// MaximizePassing against the synthesized test.
    Green,
    /// MaximizePassing against the structural ladder.
    Split,
}

impl SolveStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            SolveStage::Fix => "fix",
            SolveStage::Pin => "pin",
            SolveStage::Green => "green",
            SolveStage::Split => "split",
        }
    }
}

/// Stage-labeled round observer for the whole composition.
pub type SolveRoundObserver = Arc<dyn Fn(SolveStage, &RoundSummary) + Send + Sync>;

/// The path the dispatch actually took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolvePath {
    Fix,
    PinThenGreen,
    Pin,
    Split,
}

impl SolvePath {
    pub fn as_str(&self) -> &'static str {
        match self {
            SolvePath::Fix => "fix",
            SolvePath::PinThenGreen => "pin_then_green",
            SolvePath::Pin => "pin",
            SolvePath::Split => "split",
        }
    }
}

pub struct SolveOutcome {
    pub path: SolvePath,
    /// The synthesis-stage record when the pin-then-green path ran.
    pub synthesis: Option<TrialResult>,
    /// The trial whose status IS the outcome the caller reports:
    /// the fix/split/pin trial, or the green trial after a pin. On
    /// the pin-then-green path a failed synthesis maps to
    /// `NoBaseline` here — no tests found and none could be
    /// written is the one true failure.
    pub result: TrialResult,
    pub generated_test_path: Option<PathBuf>,
    pub generated_test_content: Option<String>,
}

pub async fn solve(
    args: SolveArgs,
    backend: Arc<dyn ChatBackend>,
    observer: Option<SolveRoundObserver>,
) -> SolveOutcome {
    let SolveArgs {
        workdir,
        model,
        goal,
        verb,
        test_command,
        config,
    } = args;
    let workdir_path = workdir.path().to_path_buf();

    let stage_observer = |stage: SolveStage| -> Option<RoundObserver> {
        observer.as_ref().map(|o| {
            let o = Arc::clone(o);
            Arc::new(move |r: &RoundSummary| o(stage, r)) as RoundObserver
        })
    };

    match verb {
        Some(SolveVerb::Pin) => {
            let trial = write_failing_test(WriteFailingTestArgs {
                workdir,
                model,
                behavior: goal,
                test_file_hint: None,
                test_command,
                config,
            });
            let result = run_trial_observed(trial, backend, stage_observer(SolveStage::Pin)).await;
            let (generated_test_path, generated_test_content) =
                read_back_synthesized_test(&workdir_path, &result);
            SolveOutcome {
                path: SolvePath::Pin,
                synthesis: None,
                result,
                generated_test_path,
                generated_test_content,
            }
        }
        Some(SolveVerb::Split { max_lines }) => {
            let focus = largest_source_file(&workdir_path)
                .unwrap_or_else(|| workdir_path.display().to_string());
            let trial = split_file(SplitFileArgs {
                workdir,
                model,
                path: PathBuf::from(focus),
                max_lines,
                test_command,
                config,
            });
            let result =
                run_trial_observed(trial, backend, stage_observer(SolveStage::Split)).await;
            SolveOutcome {
                path: SolvePath::Split,
                synthesis: None,
                result,
                generated_test_path: None,
                generated_test_content: None,
            }
        }
        Some(SolveVerb::Fix) => {
            let trial = make_failing_tests_pass(MakePassingArgs {
                workdir,
                model,
                task: Some(goal),
                test_command,
                config,
            });
            let result = run_trial_observed(trial, backend, stage_observer(SolveStage::Fix)).await;
            SolveOutcome {
                path: SolvePath::Fix,
                synthesis: None,
                result,
                generated_test_path: None,
                generated_test_content: None,
            }
        }
        None => {
            // Default: try the fix path first. Its baseline run IS
            // the probe — the two "goal isn't test-shaped yet"
            // signals fall through to pin-then-green:
            //   NoBaseline        → no tests at all
            //   Reached, 0 rounds → tests exist but all pass
            // In both, the trial never mutated the tree, so the
            // force=true re-vet below accepts the same state the
            // caller's gate already vetted.
            let fix_trial = make_failing_tests_pass(MakePassingArgs {
                workdir,
                model: model.clone(),
                task: Some(goal.clone()),
                test_command: test_command.clone(),
                config: config.clone(),
            });
            let fix_result = run_trial_observed(
                fix_trial,
                Arc::clone(&backend),
                stage_observer(SolveStage::Fix),
            )
            .await;
            let goal_needs_pinning = matches!(fix_result.status, TrialStatus::NoBaseline { .. })
                || (matches!(fix_result.status, TrialStatus::Reached) && fix_result.rounds == 0);
            if !goal_needs_pinning {
                return SolveOutcome {
                    path: SolvePath::Fix,
                    synthesis: None,
                    result: fix_result,
                    generated_test_path: None,
                    generated_test_content: None,
                };
            }

            let workdir = match Workdir::check_safe(workdir_path.clone(), true) {
                Ok(w) => w,
                Err(e) => {
                    return SolveOutcome {
                        path: SolvePath::PinThenGreen,
                        synthesis: None,
                        result: errored(&format!("re-vet workdir for pin stage: {e}")),
                        generated_test_path: None,
                        generated_test_content: None,
                    };
                }
            };
            let bdd_observer: Option<BddRoundObserver> = observer.as_ref().map(|o| {
                let o = Arc::clone(o);
                Arc::new(move |stage: BddStage, r: &RoundSummary| {
                    let stage = match stage {
                        BddStage::Synthesis => SolveStage::Pin,
                        BddStage::Green => SolveStage::Green,
                    };
                    o(stage, r)
                }) as BddRoundObserver
            });
            let cycle = bdd_cycle_observed(
                BddCycleArgs {
                    workdir,
                    model,
                    intent: goal.clone(),
                    test_file_hint: None,
                    task_hint: Some(goal),
                    test_command,
                    config,
                    review_mode: ReviewMode::Auto,
                },
                backend,
                bdd_observer,
            )
            .await;
            let result = match cycle.green {
                Some(green) => green,
                // Synthesis didn't Reach → no tests found and none
                // could be written. Report the contract's one true
                // failure, carrying the synthesis record alongside.
                None => TrialResult {
                    status: TrialStatus::NoBaseline {
                        reason: format!(
                            "no tests found and the goal-pinning test could not be \
                             written (synthesis ended {:?})",
                            cycle.synthesis.status
                        ),
                    },
                    tests_before: cycle.synthesis.tests_before.clone(),
                    tests_after: cycle.synthesis.tests_after.clone(),
                    rounds: cycle.synthesis.rounds,
                    trajectory: vec![],
                    diff: String::new(),
                },
            };
            SolveOutcome {
                path: SolvePath::PinThenGreen,
                synthesis: Some(cycle.synthesis),
                result,
                generated_test_path: cycle.generated_test_path,
                generated_test_content: cycle.generated_test_content,
            }
        }
    }
}

fn errored(reason: &str) -> TrialResult {
    TrialResult {
        status: TrialStatus::Errored {
            reason: reason.into(),
        },
        tests_before: Default::default(),
        tests_after: Default::default(),
        rounds: 0,
        trajectory: vec![],
        diff: String::new(),
    }
}

/// Read the synthesized test back after a standalone Pin verb, the
/// same best-effort way `bdd_cycle` does after its synthesis stage.
fn read_back_synthesized_test(
    workdir: &std::path::Path,
    result: &TrialResult,
) -> (Option<PathBuf>, Option<String>) {
    if !matches!(result.status, TrialStatus::Reached) {
        return (None, None);
    }
    let Some(path) = crate::tasks::bdd::most_recently_modified_test(workdir) else {
        return (None, None);
    };
    let content = std::fs::read_to_string(&path).ok();
    (Some(path), content)
}

/// Default focus file for the Split verb when the goal doesn't
/// resolve one: the workdir's largest source file — "split the big
/// one" is what the verb means with no other signal.
fn largest_source_file(workdir: &std::path::Path) -> Option<String> {
    crate::shared::discover_source_files(workdir)
        .into_iter()
        .max_by_key(|rel| {
            std::fs::read_to_string(workdir.join(rel))
                .map(|s| s.lines().count())
                .unwrap_or(0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeterministicChatBackend;
    use std::process::Command;
    use std::sync::Mutex;

    fn init_git(path: &std::path::Path) {
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

    fn vetted(path: &std::path::Path) -> Workdir {
        Workdir::check_safe(path.to_path_buf(), false).unwrap()
    }

    fn empty_backend() -> Arc<dyn ChatBackend> {
        Arc::new(DeterministicChatBackend::from_strs(Vec::<String>::new()))
    }

    #[tokio::test]
    async fn default_verb_falls_through_to_pin_when_no_tests() {
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());
        let outcome = solve(
            SolveArgs {
                workdir: vetted(tmp.path()),
                model: "test".into(),
                goal: "add an is_palindrome function".into(),
                verb: None,
                test_command: Some("pytest -q".into()),
                config: None,
            },
            empty_backend(),
            None,
        )
        .await;
        assert_eq!(outcome.path, SolvePath::PinThenGreen);
        // Synthesis can't succeed on an exhausted script — the
        // overall outcome is the contract's one true failure.
        assert!(
            matches!(outcome.result.status, TrialStatus::NoBaseline { .. }),
            "{:?}",
            outcome.result.status
        );
        assert!(outcome.synthesis.is_some());
    }

    #[tokio::test]
    async fn explicit_fix_verb_never_falls_through() {
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());
        let outcome = solve(
            SolveArgs {
                workdir: vetted(tmp.path()),
                model: "test".into(),
                goal: "fix it".into(),
                verb: Some(SolveVerb::Fix),
                test_command: Some("pytest -q".into()),
                config: None,
            },
            empty_backend(),
            None,
        )
        .await;
        assert_eq!(outcome.path, SolvePath::Fix);
        assert!(matches!(
            outcome.result.status,
            TrialStatus::NoBaseline { .. }
        ));
        assert!(outcome.synthesis.is_none());
    }

    #[tokio::test]
    async fn no_tests_repo_pins_by_import_error_then_goes_green() {
        // The spec's done-means #3 shape end-to-end: "add an
        // is_palindrome function to utils.py", no tests anywhere.
        // The pin candidate writes the idiomatic TDD opener — a test
        // that IMPORTS the not-yet-existing function — which dies as
        // a pytest collection error. The 2026-07-07 parser fold
        // counts that as a failing test, the relaxed Red predicate
        // accepts it, and the green stage climbs from 0p/1f to 1p.
        let pytest_runs = Command::new("python3")
            .args(["-m", "pytest", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !pytest_runs {
            eprintln!("python3 -m pytest not available — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());

        let pin_test = r#"```json
{"action": "write_file", "path": "tests/test_new_behavior.py"}
```

```python
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from utils import is_palindrome

def test_is_palindrome():
    assert is_palindrome("racecar")
    assert not is_palindrome("racecars")
```"#;
        let implement = r#"```json
{"action": "write_file", "path": "utils.py"}
```

```python
def is_palindrome(s):
    return s == s[::-1]
```"#;
        let backend: Arc<dyn ChatBackend> = Arc::new(DeterministicChatBackend::from_strs(vec![
            pin_test.to_string(),
            implement.to_string(),
        ]));
        let config = TrialConfig {
            candidates_per_round: 1,
            rounds_per_trial: 2,
            max_stall_rounds: 1,
            candidate_test_timeout: std::time::Duration::from_secs(30),
            ..TrialConfig::default()
        };
        let seen: Arc<Mutex<Vec<SolveStage>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let observer: SolveRoundObserver =
            Arc::new(move |stage, _round: &RoundSummary| sink.lock().unwrap().push(stage));
        let outcome = solve(
            SolveArgs {
                workdir: vetted(tmp.path()),
                model: "test".into(),
                goal: "add an is_palindrome function to utils.py".into(),
                verb: None,
                test_command: Some("python3 -m pytest -q tests/".into()),
                config: Some(config),
            },
            backend,
            Some(observer),
        )
        .await;
        assert_eq!(outcome.path, SolvePath::PinThenGreen);
        let synthesis = outcome.synthesis.expect("synthesis record");
        assert!(
            matches!(synthesis.status, TrialStatus::Reached),
            "pin stage: {:?} (tail rounds: {:?})",
            synthesis.status,
            synthesis.trajectory
        );
        assert!(
            matches!(outcome.result.status, TrialStatus::Reached),
            "green stage: {:?}",
            outcome.result.status
        );
        assert_eq!(outcome.result.tests_after.failed, 0);
        assert!(outcome.result.tests_after.passed >= 1);
        assert!(
            std::fs::read_to_string(tmp.path().join("utils.py"))
                .unwrap()
                .contains("is_palindrome"),
            "implementation landed in the tree"
        );
        let stages = seen.lock().unwrap();
        assert!(stages.contains(&SolveStage::Pin), "{stages:?}");
        assert!(stages.contains(&SolveStage::Green), "{stages:?}");
    }

    #[tokio::test]
    async fn observer_receives_stage_labeled_rounds() {
        // A workdir with one failing test and a backend whose
        // scripts never parse: the fix trial runs real rounds that
        // all stall, and each round must reach the observer with
        // the Fix stage label.
        if Command::new("pytest").arg("--version").output().is_err() {
            eprintln!("pytest not on PATH — skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());
        std::fs::write(tmp.path().join("calc.py"), "def add(a, b):\n    return 0\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("tests")).unwrap();
        std::fs::write(
            tmp.path().join("tests/test_calc.py"),
            "import sys, os\nsys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))\n\
             from calc import add\n\ndef test_add(): assert add(1, 2) == 3\n",
        )
        .unwrap();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "-A"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["commit", "-m", "fixture"])
            .output();

        let seen: Arc<Mutex<Vec<(SolveStage, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let observer: SolveRoundObserver = Arc::new(move |stage, round: &RoundSummary| {
            sink.lock().unwrap().push((stage, round.round));
        });
        let garbage = "no json action block here".to_string();
        let backend: Arc<dyn ChatBackend> = Arc::new(DeterministicChatBackend::from_strs(vec![
            garbage.clone(),
            garbage.clone(),
            garbage.clone(),
            garbage,
        ]));
        let config = TrialConfig {
            candidates_per_round: 2,
            rounds_per_trial: 2,
            max_stall_rounds: 1,
            candidate_test_timeout: std::time::Duration::from_secs(20),
            ..TrialConfig::default()
        };
        let outcome = solve(
            SolveArgs {
                workdir: vetted(tmp.path()),
                model: "test".into(),
                goal: "make add correct".into(),
                verb: None,
                test_command: Some("pytest -q tests/".into()),
                config: Some(config),
            },
            backend,
            Some(observer),
        )
        .await;
        assert_eq!(outcome.path, SolvePath::Fix, "{:?}", outcome.result.status);
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "observer never fired");
        assert!(
            seen.iter().all(|(stage, _)| *stage == SolveStage::Fix),
            "unexpected stages: {seen:?}"
        );
    }
}
