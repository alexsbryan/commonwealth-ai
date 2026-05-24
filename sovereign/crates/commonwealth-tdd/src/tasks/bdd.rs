//! BDD cycle — natural-language intent → synthesized failing test
//! → driven implementation.
//!
//! The cycle composes two existing trials: [`write_failing_test`]
//! under `Polarity::GenerateOneFailing` materializes the spec from
//! the user's intent; [`make_failing_tests_pass`] under
//! `Polarity::MaximizePassing` drives the implementation to green.
//! Neither requires new core machinery — both run through the same
//! [`run_trial`](crate::trial::run_trial) function the unified
//! solver shipped.
//!
//! Two modes:
//!
//! - [`ReviewMode::Auto`] — run synthesis, run green, return both.
//!   Default for automation (Pi extension, CI hooks).
//! - [`ReviewMode::PauseAfterSynthesis`] — run synthesis, return
//!   the test diff for the caller to review, stop before green.
//!   The caller resumes via a separate trial when they've accepted
//!   the synthesized test.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backend::ChatBackend;
use crate::tasks::make_passing::{make_failing_tests_pass, MakePassingArgs};
use crate::tasks::write_failing_test::{write_failing_test, WriteFailingTestArgs};
use crate::trial::run_trial;
use crate::types::{TrialConfig, TrialResult, TrialStatus};
use crate::workdir::Workdir;

pub struct BddCycleArgs {
    pub workdir: Workdir,
    pub model: String,
    /// User-facing natural-language description of the behavior we
    /// want. The synthesis stage uses this verbatim as the
    /// `behavior` arg to `write_failing_test`.
    pub intent: String,
    /// Optional hint at the path where the new test should land.
    /// When `None`, the framework adapter picks a path from the
    /// detected convention.
    pub test_file_hint: Option<String>,
    /// Optional hint passed to the green stage's prompt — e.g.
    /// "this is a cache eviction implementation; preserve the
    /// existing public API." When `None`, the green stage uses
    /// the default "make failing tests pass" prompt.
    pub task_hint: Option<String>,
    /// Optional override for the test command. When `None`, both
    /// stages auto-detect from the workdir's framework markers.
    pub test_command: Option<String>,
    /// Optional config override applied to both stages.
    pub config: Option<TrialConfig>,
    pub review_mode: ReviewMode,
}

#[derive(Debug, Clone, Copy)]
pub enum ReviewMode {
    /// Synthesize the test, then run the green stage automatically.
    /// Default for automation.
    Auto,
    /// Synthesize the test, return the diff, stop. Caller picks up
    /// from there.
    PauseAfterSynthesis,
}

pub struct BddCycleResult {
    pub synthesis: TrialResult,
    /// `Some(green_result)` when the green stage ran (Auto mode +
    /// synthesis Reached). `None` otherwise.
    pub green: Option<TrialResult>,
    /// Path the synthesized test file ended up at, when synthesis
    /// succeeded and the workdir reflects the new test.
    pub generated_test_path: Option<PathBuf>,
    /// Body of the synthesized test, when synthesis succeeded.
    pub generated_test_content: Option<String>,
}

pub async fn bdd_cycle(
    args: BddCycleArgs,
    backend: Arc<dyn ChatBackend>,
) -> BddCycleResult {
    let BddCycleArgs {
        workdir,
        model,
        intent,
        test_file_hint,
        task_hint,
        test_command,
        config,
        review_mode,
    } = args;

    // Hold the workdir's path before move; we need it for the
    // post-synthesis test-file read and for re-constructing the
    // workdir for the green stage.
    let workdir_path = workdir.path().to_path_buf();

    // Stage 1: synthesis. GenerateOneFailing polarity drives the
    // model to write a single test that fails on current code.
    let synthesis_trial = write_failing_test(WriteFailingTestArgs {
        workdir,
        model: model.clone(),
        behavior: intent,
        test_file_hint: test_file_hint.clone(),
        test_command: test_command.clone(),
        config: config.clone(),
    });
    let synthesis = run_trial(synthesis_trial, Arc::clone(&backend)).await;

    let (generated_test_path, generated_test_content) =
        if matches!(synthesis.status, TrialStatus::Reached) {
            // Synthesis succeeded — the failing test now lives in
            // the workdir. Read it back so the caller can render it
            // for review or persist it elsewhere.
            let path = test_file_hint
                .as_ref()
                .map(|p| workdir_path.join(p))
                .unwrap_or_else(|| {
                    // Best-effort: no hint, scan tests/ for the
                    // most-recent .py / .rs / .ts / .go file.
                    most_recently_modified_test(&workdir_path)
                        .unwrap_or_else(|| workdir_path.join("tests"))
                });
            let content = std::fs::read_to_string(&path).ok();
            (Some(path), content)
        } else {
            (None, None)
        };

    // PauseAfterSynthesis or synthesis didn't Reach → stop here.
    if !matches!(review_mode, ReviewMode::Auto)
        || !matches!(synthesis.status, TrialStatus::Reached)
    {
        return BddCycleResult {
            synthesis,
            green: None,
            generated_test_path,
            generated_test_content,
        };
    }

    // Stage 2: green. Re-vet the workdir (synthesis mutated it via
    // the test write; force=true accepts the now-dirty state since
    // the synthesis stage already vetted it once at the boundary).
    let green_workdir = match Workdir::check_safe(workdir_path.clone(), true) {
        Ok(w) => w,
        Err(e) => {
            // Synthesis succeeded but the workdir gate refused
            // re-vetting (e.g., race wrote a sentinel file outside
            // the gate's accept set). Surface as a failed green.
            return BddCycleResult {
                synthesis,
                green: Some(TrialResult {
                    status: TrialStatus::Errored {
                        reason: format!("re-vet workdir for green stage: {e}"),
                    },
                    tests_before: Default::default(),
                    tests_after: Default::default(),
                    rounds: 0,
                    trajectory: vec![],
                    diff: String::new(),
                }),
                generated_test_path,
                generated_test_content,
            };
        }
    };
    let green_trial = make_failing_tests_pass(MakePassingArgs {
        workdir: green_workdir,
        model,
        task: task_hint,
        test_command,
        config,
    });
    let green = run_trial(green_trial, backend).await;

    BddCycleResult {
        synthesis,
        green: Some(green),
        generated_test_path,
        generated_test_content,
    }
}

/// Walk `workdir/tests/` (or `workdir/` for go) and return the
/// most-recently-modified test file path. Best-effort — used only
/// for surfacing the synthesized test back to the caller when no
/// `test_file_hint` was supplied.
fn most_recently_modified_test(workdir: &std::path::Path) -> Option<PathBuf> {
    use std::time::SystemTime;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let candidates = [workdir.join("tests"), workdir.to_path_buf()];
    for dir in &candidates {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let s = name.to_string_lossy();
            let looks_like_test = s.starts_with("test_")
                || s.ends_with("_test.go")
                || s.ends_with(".test.ts")
                || s.ends_with(".test.js");
            if !looks_like_test {
                continue;
            }
            let mtime = entry.metadata().and_then(|m| m.modified()).ok();
            if let Some(t) = mtime {
                match &best {
                    Some((b, _)) if *b >= t => {}
                    _ => best = Some((t, p)),
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DeterministicChatBackend;
    use std::process::Command;

    fn init_git(path: &std::path::Path) {
        let _ = Command::new("git").arg("-C").arg(path).arg("init").arg("--initial-branch=main").output();
        let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.email", "t@t.t"]).output();
        let _ = Command::new("git").arg("-C").arg(path).args(["config", "user.name", "t"]).output();
        let _ = Command::new("git")
            .arg("-C").arg(path)
            .args(["commit", "--allow-empty", "-m", "init"])
            .output();
    }

    #[tokio::test]
    async fn pause_mode_skips_green_stage() {
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());
        let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
        let backend: Arc<dyn ChatBackend> = Arc::new(DeterministicChatBackend::from_strs(
            Vec::<String>::new(),
        ));
        let r = bdd_cycle(
            BddCycleArgs {
                workdir,
                model: "test".into(),
                intent: "anything".into(),
                test_file_hint: None,
                task_hint: None,
                test_command: Some("pytest -q".into()),
                config: None,
                review_mode: ReviewMode::PauseAfterSynthesis,
            },
            backend,
        )
        .await;
        // Synthesis fails (no tests in workdir → NoBaseline), so
        // PauseAfterSynthesis would skip green anyway. Either way,
        // green must be None.
        assert!(r.green.is_none(), "PauseAfterSynthesis must not run green");
    }

    #[tokio::test]
    async fn auto_mode_skips_green_when_synthesis_did_not_reach() {
        let tmp = tempfile::tempdir().unwrap();
        init_git(tmp.path());
        let workdir = Workdir::check_safe(tmp.path().to_path_buf(), false).unwrap();
        let backend: Arc<dyn ChatBackend> = Arc::new(DeterministicChatBackend::from_strs(
            Vec::<String>::new(),
        ));
        let r = bdd_cycle(
            BddCycleArgs {
                workdir,
                model: "test".into(),
                intent: "anything".into(),
                test_file_hint: None,
                task_hint: None,
                test_command: Some("pytest -q".into()),
                config: None,
                review_mode: ReviewMode::Auto,
            },
            backend,
        )
        .await;
        // Synthesis didn't Reach → green is skipped even in Auto.
        // This is the "don't run green against a workdir whose test
        // wasn't synthesized" invariant.
        assert!(
            r.green.is_none(),
            "Auto must skip green when synthesis didn't Reach"
        );
        assert!(!matches!(r.synthesis.status, TrialStatus::Reached));
    }
}
