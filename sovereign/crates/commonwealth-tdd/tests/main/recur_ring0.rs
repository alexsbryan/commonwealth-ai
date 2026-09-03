// SPDX-License-Identifier: AGPL-3.0-or-later
//! rec-1 ring 0: the explicit-stack driver over a scripted evaluator and a
//! REAL oracle (pytest), real worktrees, real merges. One plant per
//! mechanism — see the fixture. Bars are pre-registered in
//! `.sovereign/features/rec-1-explicit-stack/order.md`.
//!
//! The scripted evaluator is a pure function of (request, worktree), which
//! is what makes the restart and determinism bars meaningful: nothing in
//! the evaluator remembers the sequence.

use commonwealth_tdd::recur::{
    driver::delivered_to, Decider, Driver, DriverConfig, EvalRequest, EvalResponse, Event,
    GoalCatalog, GoalId, GoalPath, ScriptedEvaluator, StackState,
};
use kernel_types::Verdict;
use std::path::PathBuf;

use crate::recur_fixture::{count, fixture, g, pytest_available, root_path, strip_paths};

// ── the scripted evaluator: a pure function of (request, worktree) ───────

fn chain_bug_in(observation: &str) -> Option<u8> {
    observation
        .find(" bug")
        .and_then(|i| observation[..i].chars().last())
        .and_then(|c| c.to_digit(10))
        .map(|d| d as u8)
}

fn script(req: &EvalRequest) -> EvalResponse {
    let goal = req.goal().0.as_str();
    let obs = req.observation.as_str();
    match goal {
        "tests" => EvalResponse::Split {
            children: [
                "tests/test_chain.py",
                "tests/test_top.py",
                "tests/test_cycle.py",
                "tests/test_net.py",
            ]
            .map(g)
            .to_vec(),
        },
        "tests/test_chain.py" => EvalResponse::Push {
            goal: g("tests/test_chain.py::test_c1"),
        },
        _ if goal.starts_with("tests/test_chain.py::test_c") => {
            let mine: u8 = goal.chars().last().unwrap().to_digit(10).unwrap() as u8;
            match chain_bug_in(obs) {
                Some(b) if b == mine => {
                    // Remove MY raise from the file as it stands in the worktree.
                    let src = std::fs::read_to_string(req.worktree.join("calc/chain.py")).unwrap();
                    let fixed: String = src
                        .lines()
                        .filter(|l| !l.contains(&format!("c{mine} bug")))
                        .map(|l| format!("{l}\n"))
                        .collect();
                    EvalResponse::Edit {
                        path: "calc/chain.py".into(),
                        content: fixed,
                    }
                }
                Some(b) => EvalResponse::Push {
                    goal: g(&format!("tests/test_chain.py::test_c{b}")),
                },
                None => EvalResponse::GiveUp {
                    reason: "no chain bug in observation".into(),
                },
            }
        }
        "tests/test_top.py" => EvalResponse::Split {
            children: [
                "tests/test_top.py::test_f",
                "tests/test_top.py::test_g",
                "tests/test_top.py::test_k",
            ]
            .map(g)
            .to_vec(),
        },
        "tests/test_top.py::test_f" | "tests/test_top.py::test_g" => EvalResponse::Push {
            goal: g("tests/test_h.py::test_base"),
        },
        "tests/test_h.py::test_base" => EvalResponse::Edit {
            path: "calc/h.py".into(),
            content: "def base(a, b):\n    return a + b\n".into(),
        },
        // THE TRAP: locally correct, wrong once `base` is fixed. Green in
        // its branch, red only in the merge.
        "tests/test_top.py::test_k" => EvalResponse::Edit {
            path: "calc/k.py".into(),
            content: "from .h import base\n\ndef k(x):\n    return base(x, 2) + 3\n".into(),
        },
        "tests/test_cycle.py" => EvalResponse::Push {
            goal: g("tests/test_cycle.py::test_cyc_a"),
        },
        "tests/test_cycle.py::test_cyc_a" => EvalResponse::Push {
            goal: g("tests/test_cycle.py::test_cyc_b"),
        },
        "tests/test_cycle.py::test_cyc_b" => match &req.refused {
            None => EvalResponse::Push {
                goal: g("tests/test_cycle.py::test_cyc_a"),
            },
            Some(_) => EvalResponse::GiveUp {
                reason: "cycle: a needs b needs a".into(),
            },
        },
        other => EvalResponse::GiveUp {
            reason: format!("no script for {other}"),
        },
    }
}

fn evaluator() -> ScriptedEvaluator {
    ScriptedEvaluator::new(script)
}

fn cfg(scratch: PathBuf) -> DriverConfig {
    DriverConfig::pytest(scratch)
}

fn evaluated(state: &StackState, goal: &str) -> Vec<(usize, Verdict)> {
    state
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Evaluated { path, verdict, .. } if path.leaf().0 == goal => {
                Some((path.depth(), *verdict))
            }
            _ => None,
        })
        .collect()
}

// ── bars ─────────────────────────────────────────────────────────────────

/// Occurs · memo · combine (the trap) · flat frame — one full run.
#[tokio::test]
async fn ring0_full_run_meets_the_structural_bars() {
    if !pytest_available() {
        eprintln!("python3 -m pytest unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, false);
    let mut d = Driver::start(
        &wd,
        g("tests"),
        cfg(tmp.path().join("scratch")),
        evaluator(),
    )
    .unwrap();
    let root = d.run().await.unwrap();
    let st = d.state().clone();
    let dump = serde_json::to_string_pretty(&st.events).unwrap();

    // Root: Failed, because the merge trap and the cycle both fail and the
    // net leaf could not be judged. Fold is worst-rank.
    assert_eq!(root.verdict, Verdict::Failed, "{dump}");

    // Slot verdicts delivered to the root Combine, in order.
    let slots = delivered_to(&st, &root_path());
    let by_goal = |name: &str| slots.iter().find(|v| v.goal.0 == name).map(|v| v.verdict);
    assert_eq!(
        by_goal("tests/test_chain.py"),
        Some(Verdict::Passed),
        "{dump}"
    );
    assert_eq!(
        by_goal("tests/test_top.py"),
        Some(Verdict::Failed),
        "{dump}"
    );
    assert_eq!(
        by_goal("tests/test_cycle.py"),
        Some(Verdict::Failed),
        "{dump}"
    );
    assert_eq!(
        by_goal("tests/test_net.py"),
        Some(Verdict::CouldNotJudge),
        "{dump}"
    );

    // BAR occurs check: exactly one refusal, zero pushes of an on-path goal,
    // and the cycle resolved (we are here, so it did not hang).
    assert_eq!(
        count(&st, |e| matches!(e, Event::Refused { .. })),
        1,
        "{dump}"
    );
    for e in &st.events {
        if let Event::Pushed { from, goal } = e {
            assert!(
                !from.contains(goal),
                "pushed an on-path goal: {from} <- {goal}"
            );
        }
    }

    // BAR memo: test_h evaluated exactly once; the second parent is a hit.
    assert_eq!(
        evaluated(&st, "tests/test_h.py::test_base").len(),
        1,
        "{dump}"
    );
    assert_eq!(
        count(
            &st,
            |e| matches!(e, Event::MemoHit { path, .. } if path.leaf().0 == "tests/test_h.py::test_base")
        ),
        1,
        "{dump}"
    );

    // BAR combine: every test_top sibling passed in its branch; the trap is
    // caught in the Combine frame and nowhere else.
    let top = delivered_to(&st, &root_path().child(g("tests/test_top.py")));
    assert_eq!(top.len(), 3, "{dump}");
    assert!(top.iter().all(|v| v.verdict == Verdict::Passed), "{dump}");
    let merged = st.events.iter().find_map(|e| match e {
        Event::Merged {
            path,
            verdict,
            reason,
            ..
        } if path.leaf().0 == "tests/test_top.py" => Some((*verdict, reason.clone())),
        _ => None,
    });
    let (mv, reason) = merged.expect("a Merged event for test_top");
    assert_eq!(mv, Verdict::Failed, "{dump}");
    assert!(reason.starts_with("passed in every branch"), "{reason}");

    // The chain went depth 6 (tests > chain.py > c1 > c2 > c3 > c4).
    assert_eq!(st.max_depth(), 6, "{dump}");

    // BAR flat frame: prompt bytes at depth >= 5 within 512 of depth <= 2.
    let sizes = d.evaluator().prompt_sizes();
    let max_at = |pred: &dyn Fn(usize) -> bool| {
        sizes
            .iter()
            .filter(|(d, _)| pred(*d))
            .map(|(_, b)| *b)
            .max()
            .unwrap_or(0)
    };
    let shallow = max_at(&|d| d <= 2);
    let deep = max_at(&|d| d >= 5);
    eprintln!(
        "flat-frame: shallow(depth<=2)={shallow} bytes, deep(depth>=5)={deep} bytes, asks={}",
        sizes.len()
    );
    assert!(deep > 0 && shallow > 0, "{sizes:?}");
    assert!(
        deep <= shallow + 512,
        "deep {deep} vs shallow {shallow}: {sizes:?}"
    );
}

/// Propagation: a root of {chain, net} is could-not-judge, never passed.
#[tokio::test]
async fn ring0_could_not_judge_propagates_to_the_root() {
    if !pytest_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, false);
    let ev = ScriptedEvaluator::new(|req: &EvalRequest| {
        if req.goal().0 == "tests" {
            EvalResponse::Split {
                children: vec![g("tests/test_chain.py"), g("tests/test_net.py")],
            }
        } else {
            script(req)
        }
    });
    let mut d = Driver::start(&wd, g("tests"), cfg(tmp.path().join("scratch")), ev).unwrap();
    let root = d.run().await.unwrap();
    let dump = serde_json::to_string_pretty(&d.state().events).unwrap();
    assert_eq!(root.verdict, Verdict::CouldNotJudge, "{dump}");
    let slots = delivered_to(d.state(), &root_path());
    assert!(slots.iter().any(|v| v.verdict == Verdict::Passed), "{dump}");
    assert!(
        slots.iter().any(|v| v.verdict == Verdict::CouldNotJudge),
        "{dump}"
    );
}

/// Restart · determinism: a run killed after 4 steps and resumed from the
/// stack file ends with the same result AND the same event log as an
/// uninterrupted run on a second fixture.
#[tokio::test]
async fn ring0_resumes_from_the_stack_file_and_two_runs_agree() {
    if !pytest_available() {
        return;
    }
    let run_uninterrupted = async {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let wd = fixture(&repo, false);
        let mut d = Driver::start(
            &wd,
            g("tests"),
            cfg(tmp.path().join("scratch")),
            evaluator(),
        )
        .unwrap();
        let v = d.run().await.unwrap();
        (v, strip_paths(d.state()))
    };
    let run_killed = async {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let wd = fixture(&repo, false);
        let c = cfg(tmp.path().join("scratch"));
        let mut d = Driver::start(&wd, g("tests"), c.clone(), evaluator()).unwrap();
        let early = d.run_steps(4).await.unwrap();
        assert!(
            early.is_none(),
            "finished in 4 steps: {early:?}\n{}",
            serde_json::to_string_pretty(&d.state().events).unwrap()
        );
        let steps_before = d.state().steps;
        drop(d);
        let mut d = Driver::<ScriptedEvaluator>::resume(c, evaluator()).unwrap();
        assert_eq!(d.state().steps, steps_before);
        let v = d.run().await.unwrap();
        (v, strip_paths(d.state()))
    };
    let (a, ea) = run_uninterrupted.await;
    let (b, eb) = run_killed.await;
    assert_eq!(a, b);
    assert_eq!(ea, eb);
}

/// The write boundary: an edit that does not parse is NOT written, and the
/// frame's next ask carries the reason. Ring 3 produced exactly this reply —
/// a body that ran on into the frame scaffolding — and without the check it
/// landed in the file and cost two more asks.
#[tokio::test]
async fn ring0_an_unparseable_edit_is_rejected_before_it_is_written() {
    if !pytest_available() {
        eprintln!("python3 -m pytest unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, false);
    let good = "def base(a, b):\n    return a + b\n";
    let runon = format!("{good}\n## FRAME\ngoal: tests/test_h.py::test_base\nasks_left: 1\n");
    let ev = ScriptedEvaluator::new(move |req: &EvalRequest| {
        // Second ask only once the driver has told us why the first failed.
        let content = if req.rejected.is_some() {
            good
        } else {
            runon.as_str()
        };
        EvalResponse::Edit {
            path: "calc/h.py".into(),
            content: content.to_string(),
        }
    });
    let mut d = Driver::start(
        &wd,
        g("tests/test_h.py::test_base"),
        cfg(tmp.path().join("scratch")),
        ev,
    )
    .unwrap();
    let root = d.run().await.unwrap();
    let st = d.state().clone();
    let dump = serde_json::to_string_pretty(&st.events).unwrap();

    assert_eq!(root.verdict, Verdict::Passed, "{dump}");
    // Exactly one rejection, naming the file and a parse error.
    let rejections: Vec<&Event> = st
        .events
        .iter()
        .filter(|e| matches!(e, Event::Rejected { .. }))
        .collect();
    assert_eq!(rejections.len(), 1, "{dump}");
    let Event::Rejected { file, errors, .. } = rejections[0] else {
        unreachable!()
    };
    assert_eq!(file, "calc/h.py");
    assert!(errors.contains("calc/h.py:"), "{errors}");
    // The run-on body never reached the file.
    let on_disk = std::fs::read_to_string(repo.join("calc/h.py")).unwrap();
    assert_eq!(on_disk, good, "the rejected body was written anyway");
}

/// The driver decomposes on its own, and that is what reaches the trap.
///
/// The model does not split (measured, ring 4): it edits at the root, and
/// `Combine` is never entered. But the oracle has just run and already knows
/// that more than one part failed, so decomposability is a question code can
/// answer. Here the evaluator is scripted to NEVER split — every move it
/// makes is an edit — and the tree recursion happens anyway, all the way to
/// the merge that catches what neither branch could see.
#[tokio::test]
async fn ring0_the_driver_splits_a_multi_failure_goal_without_being_asked() {
    if !pytest_available() {
        eprintln!("python3 -m pytest unavailable — SKIPPED, this run verified nothing");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let wd = fixture(&repo, false);
    let top = g("tests/test_top.py");
    let mut cfg = cfg(tmp.path().join("scratch"));
    cfg.catalog = GoalCatalog::from_tree(
        [
            "tests/test_top.py::test_f",
            "tests/test_top.py::test_g",
            "tests/test_top.py::test_k",
        ]
        .map(|c| (GoalId::new(c), top.clone()))
        .to_vec(),
    );
    // An evaluator that CANNOT split: the goal the driver decomposes is the
    // one move this script refuses to make. Without that the assertion would
    // pass on a split the script itself would have asked for.
    let never_splits = ScriptedEvaluator::new(|req: &EvalRequest| match script(req) {
        EvalResponse::Split { .. } => EvalResponse::GiveUp {
            reason: "this evaluator never splits".into(),
        },
        other => other,
    });
    let mut d = Driver::start(&wd, top.clone(), cfg, never_splits).unwrap();
    let root = d.run().await.unwrap();
    let st = d.state().clone();
    let dump = serde_json::to_string_pretty(&st.events).unwrap();

    // Nobody asked for the split.
    let splits: Vec<&Event> = st
        .events
        .iter()
        .filter(|e| matches!(e, Event::Split { .. }))
        .collect();
    assert_eq!(splits.len(), 1, "{dump}");
    let Event::Split { by, children, .. } = splits[0] else {
        unreachable!()
    };
    assert_eq!(*by, Decider::Driver, "{dump}");
    assert_eq!(children.len(), 3, "{dump}");

    // Every branch green on its own; the merge is where it breaks.
    let slots = delivered_to(&st, &GoalPath::root(top));
    assert_eq!(slots.len(), 3, "{dump}");
    assert!(slots.iter().all(|v| v.verdict == Verdict::Passed), "{dump}");
    assert_eq!(root.verdict, Verdict::Failed, "{dump}");
    assert!(
        root.reason.starts_with("passed in every branch"),
        "{}",
        root.reason
    );
}
