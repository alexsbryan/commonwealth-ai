// SPDX-License-Identifier: AGPL-3.0-or-later
//! The iterative interpreter. One loop, two operations: pop a goal and
//! evaluate it; deliver a value to the frame beneath it. Nothing waits. The
//! whole process state is [`StackState`], persisted to `scratch/stack.json`
//! after every step, so a killed driver resumes from the file and finishes
//! with the same result (step-granular: a kill mid-evaluation re-evaluates
//! that goal on whatever the worktree holds).
//!
//! Base case: the oracle. A goal is evaluated by RUNNING it; the evaluator
//! is only asked when the oracle is red, and it is asked for a move, never
//! a verdict. Setup errors are could-not-judge, not failed (ARCH §18.2).
//!
//! Memo: `(goal, tree hash at evaluation start)` → `(value, patch)`. Sound
//! because the evaluator is a pure function of the request and the oracle
//! of the tree. A hit applies the patch and returns the value unevaluated.
//!
//! Ring 0 simplification, on purpose: a `Combine` whose merged tree is red
//! returns that verdict rather than asking the evaluator again. That is
//! the bar ("the trap is caught in the Combine frame, and only there").

use super::evaluator::{EvalError, EvalRequest, EvalResponse, Evaluator};
use super::frame::{
    fold, Continuation, Env, Event, GoalId, GoalPath, ReturnValue, Slot, StackFrame, StackItem,
};
use super::git;
use super::RECUR_INSTRUCTION;
use crate::shared::{run_tests, Language, TestRunResult};
use crate::workdir::Workdir;
use commonwealth_agent_tools::syntax::{DynSyntaxValidator, PythonSyntaxValidator};
use kernel_types::Verdict;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone)]
pub struct DriverConfig {
    /// The depth-agnostic instruction handed to every frame. Byte-identical
    /// at every depth by construction; ring 2 pins it as one prefix family.
    pub instruction: &'static str,
    /// `{goal}` is replaced by the goal id.
    pub test_command: String,
    pub language: Language,
    /// Evaluator asks per frame — the frame-level well-founded measure.
    pub asks_per_frame: u32,
    /// Goal evaluations per run — the process-level measure.
    pub max_steps: u32,
    pub test_timeout: Duration,
    /// Holds `stack.json` and the forked worktrees. Outside the repo.
    pub scratch: PathBuf,
    /// Checked BEFORE an edit is written. A model's edit body can run on
    /// past the file (measured, ring 3: one reply carried the frame
    /// scaffolding into `calc/cyc_a.py`), and a grammar terminator does not
    /// prevent it — llguidance stops constraining once the start rule is
    /// satisfied, so the model emits the marker and keeps going. The write
    /// boundary is where the check belongs. Reuses the trial loop's seam
    /// (`Trial::syntax_validator`) rather than minting a second one.
    pub syntax_validator: Option<DynSyntaxValidator>,
}

impl std::fmt::Debug for DriverConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverConfig")
            .field("test_command", &self.test_command)
            .field("language", &self.language)
            .field("asks_per_frame", &self.asks_per_frame)
            .field("max_steps", &self.max_steps)
            .field("test_timeout", &self.test_timeout)
            .field("scratch", &self.scratch)
            .field(
                "syntax_validator",
                &self.syntax_validator.as_ref().map(|v| v.language_id()),
            )
            .finish()
    }
}

impl DriverConfig {
    pub fn pytest(scratch: PathBuf) -> Self {
        Self {
            instruction: RECUR_INSTRUCTION,
            // No bytecode, no cache dir: the memo key is a tree hash and a merge
            // is a tree merge, so nothing the oracle writes may land in either.
            // Continue past collection errors: a directory goal's observation
            // must show every failure, not only the file that cannot import
            // (the model gave up at the root otherwise — ring 2).
            test_command:
                "PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -q -p no:cacheprovider --continue-on-collection-errors {goal}".into(),
            language: Language::Python,
            asks_per_frame: 3,
            max_steps: 200,
            test_timeout: Duration::from_secs(60),
            scratch,
            syntax_validator: Some(std::sync::Arc::new(PythonSyntaxValidator::new())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoEntry {
    pub value: ReturnValue,
    pub patch: String,
}

/// The entire process. Serializable, so the process outlives the driver.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackState {
    pub stack: Vec<StackItem>,
    pub memo: BTreeMap<String, MemoEntry>,
    pub events: Vec<Event>,
    pub steps: u32,
    pub result: Option<ReturnValue>,
}

impl StackState {
    pub fn max_depth(&self) -> usize {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::Evaluated { path, .. } => Some(path.depth()),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Eval(#[from] EvalError),
    #[error("stack state: {0}")]
    State(String),
    #[error("stack empty with no result")]
    EmptyStack,
    #[error("a goal was found where a frame was expected (delivering {0})")]
    GoalAboveFrame(GoalId),
}

enum Outcome {
    Value(ReturnValue),
    /// A frame and a goal were pushed; the loop continues.
    Suspended,
}

pub struct Driver<E: Evaluator> {
    cfg: DriverConfig,
    eval: E,
    state: StackState,
}

impl<E: Evaluator> Driver<E> {
    /// Begin a process at `root`, evaluated in the workdir itself. The
    /// `Workdir` gate is the only way in (ARCH §7.1).
    pub fn start(
        workdir: &Workdir,
        root: GoalId,
        cfg: DriverConfig,
        eval: E,
    ) -> Result<Self, DriverError> {
        std::fs::create_dir_all(&cfg.scratch)?;
        let env = Env {
            worktree: workdir.path().to_path_buf(),
            branch: git::current_branch(workdir.path())?,
        };
        let state = StackState {
            stack: vec![StackItem::Goal {
                path: GoalPath::root(root),
                env,
            }],
            ..Default::default()
        };
        let d = Self { cfg, eval, state };
        d.persist()?;
        Ok(d)
    }

    /// Resume from `scratch/stack.json`.
    pub fn resume(cfg: DriverConfig, eval: E) -> Result<Self, DriverError> {
        let raw = std::fs::read_to_string(Self::state_path(&cfg))?;
        let state: StackState =
            serde_json::from_str(&raw).map_err(|e| DriverError::State(e.to_string()))?;
        Ok(Self { cfg, eval, state })
    }

    pub fn state(&self) -> &StackState {
        &self.state
    }

    pub fn evaluator(&self) -> &E {
        &self.eval
    }

    fn state_path(cfg: &DriverConfig) -> PathBuf {
        cfg.scratch.join("stack.json")
    }

    fn persist(&self) -> Result<(), DriverError> {
        let path = Self::state_path(&self.cfg);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.state)
            .map_err(|e| DriverError::State(e.to_string()))?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    fn event(&mut self, e: Event) {
        tracing::debug!(target: "recur", event = ?e);
        self.state.events.push(e);
    }

    /// Run to completion.
    pub async fn run(&mut self) -> Result<ReturnValue, DriverError> {
        loop {
            if let Some(v) = self.run_steps(u32::MAX).await? {
                return Ok(v);
            }
        }
    }

    /// Run at most `n` steps (one step = one goal popped and handled).
    /// `Some` when the process has finished.
    pub async fn run_steps(&mut self, n: u32) -> Result<Option<ReturnValue>, DriverError> {
        for _ in 0..n {
            if let Some(v) = &self.state.result {
                return Ok(Some(v.clone()));
            }
            let Some(item) = self.state.stack.pop() else {
                return Err(DriverError::EmptyStack);
            };
            match item {
                StackItem::Goal { path, env } => {
                    self.state.steps += 1;
                    let outcome = if self.state.steps > self.cfg.max_steps {
                        let steps = self.state.steps;
                        self.event(Event::Budget {
                            path: path.clone(),
                            steps,
                        });
                        Outcome::Value(ReturnValue {
                            goal: path.leaf().clone(),
                            verdict: Verdict::NeverRan,
                            reason: format!("step budget {} exhausted", self.cfg.max_steps),
                        })
                    } else {
                        let before = git::tree_hash(&env.worktree)?;
                        self.evaluate(path, env, before, self.cfg.asks_per_frame, true, None)
                            .await?
                    };
                    if let Outcome::Value(v) = outcome {
                        self.deliver(v).await?;
                    }
                }
                StackItem::Frame(f) => {
                    return Err(DriverError::State(format!(
                        "frame {} popped without a value",
                        f.path
                    )))
                }
            }
            self.persist()?;
        }
        Ok(self.state.result.clone())
    }

    /// `Some(errors)` when the configured validator refuses this content,
    /// so the file is not written. `None` means write it — including when
    /// no validator handles the extension, which is reported as "no
    /// validator", never as "valid".
    fn reject_edit(&self, file: &str, content: &str) -> Option<String> {
        let v = self.cfg.syntax_validator.as_ref()?;
        if !v.language_extensions().iter().any(|e| file.ends_with(e)) {
            return None;
        }
        let errors = v.check_file(std::path::Path::new(file), content);
        if errors.is_empty() {
            return None;
        }
        Some(
            errors
                .iter()
                .map(|e| format!("{}:{}:{}: {}", file, e.line, e.col, e.message))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    fn command_for(&self, goal: &GoalId) -> String {
        self.cfg.test_command.replace("{goal}", &goal.0)
    }

    async fn oracle(&self, goal: &GoalId, env: &Env) -> TestRunResult {
        run_tests(
            &env.worktree,
            &self.command_for(goal),
            self.cfg.language,
            self.cfg.test_timeout,
        )
        .await
    }

    /// Passed / Failed from the counts; CouldNotJudge when the run could not
    /// judge (setup error, or nothing ran at all). Only Passed is terminal on
    /// its own — the other two are observations until the frame is out of
    /// moves.
    fn classify(run: &TestRunResult) -> Verdict {
        let p = &run.parsed;
        let setup = p.failed_names.iter().any(|n| n.starts_with("<setup error"));
        if setup || p.total == 0 {
            Verdict::CouldNotJudge
        } else if p.failed == 0 {
            Verdict::Passed
        } else {
            Verdict::Failed
        }
    }

    fn memo_key(goal: &GoalId, before: &str) -> String {
        format!("{goal}@{before}")
    }

    fn memoize(
        &mut self,
        goal: &GoalId,
        env: &Env,
        before: &str,
        value: &ReturnValue,
    ) -> Result<(), DriverError> {
        let after = git::tree_hash(&env.worktree)?;
        let patch = git::diff_trees(&env.worktree, before, &after)?;
        self.state.memo.insert(
            Self::memo_key(goal, before),
            MemoEntry {
                value: value.clone(),
                patch,
            },
        );
        Ok(())
    }

    /// Evaluate one goal in its env. `fresh` = first entry (consult the memo);
    /// a `Verify` resume passes `false` and its remaining asks.
    async fn evaluate(
        &mut self,
        path: GoalPath,
        env: Env,
        before: String,
        mut asks_left: u32,
        fresh: bool,
        mut sub_result: Option<ReturnValue>,
    ) -> Result<Outcome, DriverError> {
        let goal = path.leaf().clone();
        let key = Self::memo_key(&goal, &before);
        if fresh {
            if let Some(m) = self.state.memo.get(&key).cloned() {
                git::apply(&env.worktree, &m.patch)?;
                self.event(Event::MemoHit {
                    path: path.clone(),
                    key,
                });
                return Ok(Outcome::Value(ReturnValue { goal, ..m.value }));
            }
        }
        let mut refused: Option<GoalId> = None;
        let mut rejected: Option<String> = None;
        let mut asks = 0u32;
        loop {
            let run = self.oracle(&goal, &env).await;
            let verdict = Self::classify(&run);
            let terminal = |verdict: Verdict, reason: String| ReturnValue {
                goal: goal.clone(),
                verdict,
                reason,
            };
            // Green is the base case. Anything else is an observation the
            // evaluator gets to move on; the frame's verdict when it is out
            // of moves is whatever the LAST oracle run showed — Failed, or
            // CouldNotJudge for a setup error (ARCH §18.2, never collapsed).
            let done = match verdict {
                Verdict::Passed => Some(terminal(verdict, "oracle green".into())),
                _ if asks_left == 0 => Some(terminal(
                    verdict,
                    format!(
                        "asks exhausted, last run {}: {}",
                        verdict.as_str(),
                        last_line(&run.tail)
                    ),
                )),
                _ => None,
            };
            if let Some(v) = done {
                self.memoize(&goal, &env, &before, &v)?;
                self.event(Event::Evaluated {
                    path,
                    verdict: v.verdict,
                    asks,
                    reason: v.reason.clone(),
                });
                return Ok(Outcome::Value(v));
            }
            asks_left -= 1;
            asks += 1;
            let req = EvalRequest {
                instruction: self.cfg.instruction,
                path: path.clone(),
                on_stack: path.goals().to_vec(),
                observation: run.tail.clone(),
                refused: refused.take(),
                rejected: rejected.take(),
                sub_result: sub_result.take(),
                asks_left,
                worktree: env.worktree.clone(),
            };
            match self.eval.evaluate(&req).await? {
                EvalResponse::Push { goal: sub } => {
                    if path.contains(&sub) {
                        self.event(Event::Refused {
                            from: path.clone(),
                            goal: sub.clone(),
                        });
                        refused = Some(sub);
                        continue;
                    }
                    self.event(Event::Pushed {
                        from: path.clone(),
                        goal: sub.clone(),
                    });
                    let child = path.child(sub);
                    self.state.stack.push(StackItem::Frame(StackFrame {
                        path,
                        env: env.clone(),
                        before,
                        k: Continuation::Verify { asks_left },
                    }));
                    self.state.stack.push(StackItem::Goal { path: child, env });
                    return Ok(Outcome::Suspended);
                }
                EvalResponse::Edit {
                    path: file,
                    content,
                } => {
                    let target = env.worktree.join(&file);
                    if let Some(errors) = self.reject_edit(&file, &content) {
                        self.event(Event::Rejected {
                            from: path.clone(),
                            file: file.clone(),
                            errors: errors.clone(),
                        });
                        rejected = Some(errors);
                        continue;
                    }
                    if let Some(p) = target.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    std::fs::write(target, content)?;
                }
                EvalResponse::Split { children } => {
                    git::commit_all(&env.worktree, &format!("recur: split {goal}"))?;
                    let base = git::head(&env.worktree)?;
                    let mut slots = Vec::with_capacity(children.len());
                    for c in &children {
                        let cp = path.child(c.clone());
                        let branch = format!("recur/{}", cp.short_slug());
                        let wt = self.cfg.scratch.join("wt").join(cp.short_slug());
                        git::add_worktree(&env.worktree, &wt, &branch, &base)?;
                        slots.push(Slot {
                            goal: c.clone(),
                            env: Env {
                                worktree: wt,
                                branch,
                            },
                            value: None,
                        });
                    }
                    self.event(Event::Split {
                        from: path.clone(),
                        children: children.clone(),
                    });
                    let first = slots[0].clone();
                    self.state.stack.push(StackItem::Frame(StackFrame {
                        path: path.clone(),
                        env,
                        before,
                        k: Continuation::Combine { slots, next: 1 },
                    }));
                    self.state.stack.push(StackItem::Goal {
                        path: path.child(first.goal),
                        env: first.env,
                    });
                    return Ok(Outcome::Suspended);
                }
                EvalResponse::GiveUp { reason } => {
                    let v = terminal(verdict, format!("gave up ({}): {reason}", verdict.as_str()));
                    self.memoize(&goal, &env, &before, &v)?;
                    self.event(Event::Evaluated {
                        path,
                        verdict: v.verdict,
                        asks,
                        reason: v.reason.clone(),
                    });
                    return Ok(Outcome::Value(v));
                }
            }
        }
    }

    /// Deliver a value to the frame beneath it, and keep delivering while
    /// frames keep returning values. Stops when a frame suspends (pushes)
    /// or the stack is empty (the root has its value).
    async fn deliver(&mut self, mut v: ReturnValue) -> Result<(), DriverError> {
        loop {
            match self.state.stack.pop() {
                None => {
                    self.state.result = Some(v);
                    return Ok(());
                }
                Some(StackItem::Goal { path, .. }) => {
                    return Err(DriverError::GoalAboveFrame(path.leaf().clone()))
                }
                Some(StackItem::Frame(f)) => {
                    self.event(Event::Delivered {
                        to: f.path.clone(),
                        value: v.clone(),
                    });
                    match f.k.clone() {
                        Continuation::Verify { asks_left } => {
                            // Resume this frame's own evaluation whatever the
                            // sub-goal returned: the oracle re-runs the goal,
                            // and a failed push is information for the next
                            // ask, not this frame's verdict. Out of asks, the
                            // frame returns what its last oracle run showed.
                            match self
                                .evaluate(f.path, f.env, f.before, asks_left, false, Some(v))
                                .await?
                            {
                                Outcome::Value(v2) => {
                                    v = v2;
                                    continue;
                                }
                                Outcome::Suspended => return Ok(()),
                            }
                        }
                        Continuation::Combine { mut slots, next } => {
                            if let Some(s) = slots
                                .iter_mut()
                                .find(|s| s.goal == v.goal && s.value.is_none())
                            {
                                s.value = Some(v.clone());
                            }
                            if next < slots.len() {
                                let s = slots[next].clone();
                                self.state.stack.push(StackItem::Frame(StackFrame {
                                    path: f.path.clone(),
                                    env: f.env,
                                    before: f.before,
                                    k: Continuation::Combine {
                                        slots,
                                        next: next + 1,
                                    },
                                }));
                                self.state.stack.push(StackItem::Goal {
                                    path: f.path.child(s.goal),
                                    env: s.env,
                                });
                                return Ok(());
                            }
                            v = self.combine(&f, &slots).await?;
                            continue;
                        }
                    }
                }
            }
        }
    }

    /// All siblings delivered: commit each branch, merge them into the
    /// frame's env, run the goal on the merged tree, fold.
    async fn combine(
        &mut self,
        f: &StackFrame,
        slots: &[Slot],
    ) -> Result<ReturnValue, DriverError> {
        for s in slots {
            git::commit_all(&s.env.worktree, &format!("recur: leaf {}", s.goal))?;
        }
        let branches: Vec<String> = slots.iter().map(|s| s.env.branch.clone()).collect();
        let sibling_fold = fold(
            slots
                .iter()
                .filter_map(|s| s.value.as_ref().map(|v| v.verdict)),
        );
        let (verdict, reason) = match git::merge(&f.env.worktree, &branches)? {
            Err(conflict) => (
                Verdict::Failed,
                format!("merge conflict: {}", last_line(&conflict)),
            ),
            Ok(()) => {
                let run = self.oracle(f.goal(), &f.env).await;
                let merged = Self::classify(&run);
                if merged == Verdict::Passed {
                    (Verdict::Passed, "merged tree green".into())
                } else if sibling_fold == Verdict::Passed {
                    (
                        merged,
                        format!(
                            "passed in every branch, {} in merge: {}",
                            merged.as_str(),
                            last_line(&run.tail)
                        ),
                    )
                } else {
                    let slot_summary: Vec<String> = slots
                        .iter()
                        .map(|s| {
                            format!(
                                "{}={}",
                                s.goal,
                                s.value
                                    .as_ref()
                                    .map(|v| v.verdict.as_str())
                                    .unwrap_or("never-ran")
                            )
                        })
                        .collect();
                    (
                        fold([sibling_fold, merged]),
                        format!(
                            "siblings [{}], merge {}",
                            slot_summary.join(", "),
                            merged.as_str()
                        ),
                    )
                }
            }
        };
        self.event(Event::Merged {
            path: f.path.clone(),
            branches,
            verdict,
            reason: reason.clone(),
        });
        let out = ReturnValue {
            goal: f.goal().clone(),
            verdict,
            reason,
        };
        self.memoize(f.goal(), &f.env, &f.before, &out)?;
        Ok(out)
    }
}

/// The oracle's summary line with pytest's wall-time suffix (` in 0.03s`)
/// removed: a reason is part of the memoized, persisted, compared record,
/// and timing is not part of the verdict.
fn last_line(s: &str) -> String {
    let line = s
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.rfind(" in ") {
        Some(i) if line.ends_with('s') && line[i + 4..line.len() - 1].parse::<f64>().is_ok() => {
            line[..i].to_string()
        }
        _ => line.to_string(),
    }
}

/// Read-only view for reports: the slot verdicts recorded under each
/// `Merged` event's path, in delivery order.
pub fn delivered_to(state: &StackState, path: &GoalPath) -> Vec<ReturnValue> {
    state
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Delivered { to, value } if to == path => Some(value.clone()),
            _ => None,
        })
        .collect()
}
