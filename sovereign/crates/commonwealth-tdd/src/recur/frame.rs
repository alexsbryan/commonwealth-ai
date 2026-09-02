// SPDX-License-Identifier: AGPL-3.0-or-later
//! The frame is a RECORD, not a context. SICP 5.4: a frame holds the
//! continuation and its free variables — nothing about how they were
//! computed. Everything here is small by construction, and the one field
//! that could grow (the goal) is a test id, because a goal that cannot be
//! named as a test has no base case and no small frame.

use kernel_types::Verdict;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A goal is a test the oracle can run: a pytest node id such as
/// `tests/test_top.py::test_f`, or a path. Identity from essence (ARCH §7.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalId(pub String);

impl GoalId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Branch- and path-safe spelling.
    pub fn slug(&self) -> String {
        self.0
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
}

impl std::fmt::Display for GoalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The goals from the root to this frame. The frame's identity, the occurs
/// check's subject, and the branch name all derive from it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalPath(pub Vec<GoalId>);

impl GoalPath {
    pub fn root(g: GoalId) -> Self {
        Self(vec![g])
    }

    pub fn child(&self, g: GoalId) -> GoalPath {
        let mut v = self.0.clone();
        v.push(g);
        GoalPath(v)
    }

    pub fn contains(&self, g: &GoalId) -> bool {
        self.0.contains(g)
    }

    /// Root is depth 1.
    pub fn depth(&self) -> usize {
        self.0.len()
    }

    pub fn leaf(&self) -> &GoalId {
        self.0.last().expect("a GoalPath is never empty")
    }

    pub fn slug(&self) -> String {
        self.0.iter().map(GoalId::slug).collect::<Vec<_>>().join("__")
    }
}

impl std::fmt::Display for GoalPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<&str> = self.0.iter().map(|g| g.0.as_str()).collect();
        f.write_str(&parts.join(" > "))
    }
}

/// Where a goal is evaluated: a git worktree on a branch. A `Verify` child
/// shares its parent's env — the sub-call runs in the caller's environment.
/// `Combine` children each fork one from the parent's commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Env {
    pub worktree: PathBuf,
    pub branch: String,
}

/// The value a frame returns. A sum, never collapsed (ARCH §18.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnValue {
    pub goal: GoalId,
    pub verdict: Verdict,
    pub reason: String,
}

/// One sibling of a `Combine`: its goal, its branch, and the slot its value
/// lands in — `None` until delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub goal: GoalId,
    pub env: Env,
    pub value: Option<ReturnValue>,
}

/// The closed set of deferred operations (ARCH §2: closed set → enum).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Continuation {
    /// `(verify goal _)`: re-run the goal once the sub-result arrives, then
    /// continue this frame's own evaluation with the asks it had left.
    Verify { asks_left: u32 },
    /// Fold sibling values, merge their branches into this env, run the goal
    /// on the merged tree. Siblings run one at a time in ring 0; `next` is
    /// the index of the sibling not yet pushed.
    Combine { slots: Vec<Slot>, next: usize },
}

/// A deferred operation awaiting a value. `before` is the tree hash when the
/// goal's evaluation began — the memo key, and the base of the memo patch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackFrame {
    pub path: GoalPath,
    pub env: Env,
    pub before: String,
    pub k: Continuation,
}

impl StackFrame {
    pub fn goal(&self) -> &GoalId {
        self.path.leaf()
    }
}

/// What the stack holds: a goal awaiting evaluation, or a frame awaiting a
/// value. The driver only ever pops a `Frame` while delivering a value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum StackItem {
    Goal { path: GoalPath, env: Env },
    Frame(StackFrame),
}

/// Fold sibling verdicts: the worst rank wins (`Verdict::rank`). So
/// {Passed, CouldNotJudge} is CouldNotJudge, and any Failed is Failed. An
/// empty fold is NeverRan — nothing was judged.
pub fn fold(verdicts: impl IntoIterator<Item = Verdict>) -> Verdict {
    verdicts
        .into_iter()
        .min_by_key(|v| v.rank())
        .unwrap_or(Verdict::NeverRan)
}

/// The glassbox log: every decision the driver takes is one event (ARCH §9).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Evaluated {
        path: GoalPath,
        verdict: Verdict,
        asks: u32,
        reason: String,
    },
    Pushed {
        from: GoalPath,
        goal: GoalId,
    },
    /// The occurs check: `goal` is already on `from`'s path.
    Refused {
        from: GoalPath,
        goal: GoalId,
    },
    Split {
        from: GoalPath,
        children: Vec<GoalId>,
    },
    MemoHit {
        path: GoalPath,
        key: String,
    },
    Merged {
        path: GoalPath,
        branches: Vec<String>,
        verdict: Verdict,
        reason: String,
    },
    Delivered {
        to: GoalPath,
        value: ReturnValue,
    },
    Budget {
        path: GoalPath,
        steps: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_worst_rank() {
        assert_eq!(fold([Verdict::Passed, Verdict::Passed]), Verdict::Passed);
        assert_eq!(
            fold([Verdict::Passed, Verdict::CouldNotJudge]),
            Verdict::CouldNotJudge
        );
        assert_eq!(
            fold([Verdict::CouldNotJudge, Verdict::Failed, Verdict::Passed]),
            Verdict::Failed
        );
        assert_eq!(fold([Verdict::NeverRan, Verdict::CouldNotJudge]), Verdict::NeverRan);
        assert_eq!(fold([]), Verdict::NeverRan);
    }

    #[test]
    fn path_contains_is_the_occurs_check() {
        let p = GoalPath::root(GoalId::new("a")).child(GoalId::new("b"));
        assert!(p.contains(&GoalId::new("a")));
        assert!(!p.contains(&GoalId::new("c")));
        assert_eq!(p.depth(), 2);
        assert_eq!(p.slug(), "a__b");
    }
}
