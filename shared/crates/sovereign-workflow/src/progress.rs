// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-step progress events + an observer callback — the Runner's glassbox seam.
//!
//! The Runner emits one [`WorkflowProgress`] event at each lifecycle point it
//! already logs to `tracing` (run start, each step's completion, a tolerant
//! `for_each`'s skipped element, each item's completion, run finish). A headless
//! caller (the CLI, the daemon trigger) attaches no observer and relies on the
//! `tracing` events; an interactive caller (the desktop run surface) attaches a
//! [`StepObserver`] that forwards each event to its UI — "watch it go" without
//! the Runner knowing anything about the UI it feeds.

use std::sync::Arc;

/// A sink for [`WorkflowProgress`] events, invoked synchronously by the Runner
/// as a run proceeds. Cheap to clone (an `Arc`); the default is `None` (the
/// headless path — events go only to `tracing`).
pub type StepObserver = Arc<dyn Fn(WorkflowProgress) + Send + Sync>;

/// One lifecycle event from a workflow run. Carries only owned, display-ready
/// fields so an observer can forward it across a process boundary (e.g. a Tauri
/// event) without reaching into the Runner's internals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowProgress {
    /// The run has enumerated its items and is about to start.
    RunStarted {
        workflow: String,
        items: usize,
        steps: usize,
    },
    /// A step finished for one item. `cached` is true when every unit of the
    /// step's work was served from the content cache (nothing re-executed).
    /// `step_index`/`total_steps` are the step's position in the per-item
    /// sequence, for a progress bar.
    StepDone {
        item: String,
        step: String,
        uses: String,
        for_each: bool,
        cached: bool,
        step_index: usize,
        total_steps: usize,
    },
    /// A tolerant `for_each` (`on_error = "skip"`) skipped a failing element.
    ElementSkipped {
        item: String,
        step: String,
        index: usize,
        error: String,
    },
    /// All steps for one item finished (`ok` = the item produced a result rather
    /// than aborting on a step error).
    ItemDone {
        item: String,
        ok: bool,
        ran: usize,
        cached: usize,
    },
    /// The whole run finished, with the item-level tallies.
    RunFinished { ok: usize, failed: usize },
}

/// Fire `ev` at `observer` if one is attached. Keeps each Runner emit site a
/// single line beside the existing `tracing` call.
pub(crate) fn emit(observer: Option<&StepObserver>, ev: WorkflowProgress) {
    if let Some(o) = observer {
        o(ev);
    }
}
