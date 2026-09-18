//! Workbench's part of the node's state — the next-edit model lane's
//! one-in-flight budget.
//!
//! DC §4.2 assigns this field to Workbench, whose home is next-edit's crate,
//! which does not exist yet. Until that move this part is scaffolding carried
//! on `AppStateInner`; the route shells read it directly rather than through
//! delegating accessors.

use std::sync::Arc;

/// Workbench's field, held as `AppStateInner::workbench`.
pub struct WorkbenchPart {
    /// One-in-flight budget for the next-edit model lane
    /// (`sovereign/docs/NEXT_EDIT.md` §4): a consult that finds the
    /// slot busy is dropped immediately (`dropped: "busy"`), never
    /// queued — ghost text and chat always win the slot.
    ///
    /// `Arc` so the permit can be acquired *owned* and moved into the
    /// task that actually runs the inference. Dropping a completion
    /// future does NOT stop the generation behind it — the engine
    /// dispatches through `spawn_blocking`, and dropping a
    /// `JoinHandle` detaches rather than cancels — so a permit tied
    /// to the route's timeout would release while llama.cpp still
    /// held the slot, and this budget would stop bounding anything.
    pub next_edit_model_slot: Arc<tokio::sync::Semaphore>,
}
