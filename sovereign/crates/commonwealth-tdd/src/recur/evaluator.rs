// SPDX-License-Identifier: AGPL-3.0-or-later
//! The primitive operation the loop calls. It sees ONE frame: the instruction,
//! the goal, the goals on the stack, and the oracle's tail. It returns ONE
//! move. It never returns a verdict — the oracle decides those.
//!
//! Ring 0: [`ScriptedEvaluator`], a pure function of the request (so it
//! survives a restart and two runs agree). Ring 2: the local model behind
//! the same trait, with `on_stack` becoming the grammar's exclusion list and
//! the instruction becoming the pinned prefix.

use super::frame::{GoalId, GoalPath};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Clone, Debug)]
pub struct EvalRequest {
    /// Byte-identical at every depth: one prefix family.
    pub instruction: &'static str,
    pub path: GoalPath,
    /// The goals on the path. In ring 2 the grammar excludes these from
    /// `push`; in ring 0 the driver refuses them and says so via `refused`.
    pub on_stack: Vec<GoalId>,
    /// The last ~1.5KB of the failing oracle run.
    pub observation: String,
    /// A push the driver refused since the last ask, if any.
    pub refused: Option<GoalId>,
    pub asks_left: u32,
    pub worktree: PathBuf,
}

impl EvalRequest {
    pub fn goal(&self) -> &GoalId {
        self.path.leaf()
    }

    /// The prompt as the model would see it. Its byte length is the
    /// "flat frame" measurement: it must not grow with depth.
    pub fn render(&self) -> String {
        let on_stack: Vec<&str> = self.on_stack.iter().map(|g| g.0.as_str()).collect();
        let refused = self
            .refused
            .as_ref()
            .map(|g| format!("\nrefused: {g} (already on the stack)"))
            .unwrap_or_default();
        format!(
            "{}\n\n## Frame\ngoal: {}\non_stack: {}\nasks_left: {}{}\n\n## Observation\n{}\n",
            self.instruction,
            self.goal(),
            on_stack.join(", "),
            self.asks_left,
            refused,
            self.observation
        )
    }
}

/// The closed set of moves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EvalResponse {
    /// Push a sub-goal; this frame becomes `(verify goal _)`.
    Push { goal: GoalId },
    /// Rewrite one file (path relative to the worktree); the oracle re-runs.
    Edit { path: String, content: String },
    /// Decompose into siblings; this frame becomes a `Combine`.
    Split { children: Vec<GoalId> },
    /// No move exists. The frame returns Failed with this reason.
    GiveUp { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("evaluator backend: {0}")]
    Backend(String),
}

#[async_trait]
pub trait Evaluator: Send + Sync {
    async fn evaluate(&self, req: &EvalRequest) -> Result<EvalResponse, EvalError>;
}

/// Ring 0: a pure function of the request. Records every rendered prompt's
/// (depth, bytes) so the flat-frame bar can be read off it.
pub struct ScriptedEvaluator {
    script: Box<dyn Fn(&EvalRequest) -> EvalResponse + Send + Sync>,
    prompts: Mutex<Vec<(usize, usize)>>,
}

impl ScriptedEvaluator {
    pub fn new(script: impl Fn(&EvalRequest) -> EvalResponse + Send + Sync + 'static) -> Self {
        Self {
            script: Box::new(script),
            prompts: Mutex::new(Vec::new()),
        }
    }

    /// `(depth, prompt bytes)` per ask, in order.
    pub fn prompt_sizes(&self) -> Vec<(usize, usize)> {
        self.prompts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[async_trait]
impl Evaluator for ScriptedEvaluator {
    async fn evaluate(&self, req: &EvalRequest) -> Result<EvalResponse, EvalError> {
        let bytes = req.render().len();
        self.prompts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((req.path.depth(), bytes));
        Ok((self.script)(req))
    }
}
