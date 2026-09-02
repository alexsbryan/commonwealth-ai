// SPDX-License-Identifier: AGPL-3.0-or-later
//! rec-1 — the explicit stack. SICP 5.4's explicit-control evaluator over a
//! model: a recursive PROCESS run by an ITERATIVE interpreter.
//!
//! The frame is a record (continuation tag + free variables + parent path),
//! the stack is a file, the driver pops and never waits, and the evaluator
//! is the primitive operation the loop calls. No session is resident while
//! a sub-goal runs — the parent exists only as a [`StackFrame`] on disk.
//!
//! Ring 0 (this module as landed): the evaluator is scripted, the oracle is
//! real (pytest through `run_tests`), worktrees and merges are real git.
//! Ring 2 puts the local model behind the same [`Evaluator`] trait and adds
//! the KV-state snapshot as a CACHE of the record — never the other way.
//!
//! Order: `.sovereign/features/rec-1-explicit-stack/order.md` (the bars).

pub mod driver;
pub mod evaluator;
pub mod frame;
pub mod git;

pub use driver::{Driver, DriverConfig, DriverError, MemoEntry, StackState};
pub use evaluator::{EvalError, EvalRequest, EvalResponse, Evaluator, ScriptedEvaluator};
pub use frame::{
    fold, Continuation, Env, Event, GoalId, GoalPath, ReturnValue, Slot, StackFrame, StackItem,
};

/// The depth-agnostic instruction. Byte-identical at every depth by
/// construction, so in ring 2 it is exactly one pinned-prefix family.
/// Asset, not literal (ARCH §6.2).
pub const RECUR_INSTRUCTION: &str = include_str!("../../assets/recur_instruction.md");
